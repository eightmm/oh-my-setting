"""One impact brief for a Git range: what a reviewer or worker should look at next to the diff.

It joins readers that already exist and adds nothing of its own to the graph:
the affected-test plan, the files that structurally depend on the changed ones,
the paths that history says usually move with them but this range left alone,
and who CODEOWNERS names for each changed path. Every list is cut to one bound
and says how much it left out. The brief is orientation: it tells a reader where
to look, and none of it is evidence that a change is safe, tested or approved.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_graph.errors import GraphError
from . import affected as project_affected
from . import history as project_history
from .blast import blast_radius
from .query import Graph

CODEOWNERS_PATHS = (".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS")
CODEOWNERS_MAX_BYTES = 1024 * 1024
NOTE = "orientation only: where to look, not evidence that the change is safe, tested or approved"


def _owner_pattern(pattern: str) -> Optional["re.Pattern[str]"]:
    """A CODEOWNERS pattern as a regex over repo-relative paths; None for one this reader does not support."""
    if not pattern or pattern.startswith("!") or "[" in pattern or "\\" in pattern:
        return None
    anchored = pattern.startswith("/") or "/" in pattern.rstrip("/")
    directory = pattern.endswith("/")
    body = pattern.strip("/")
    if not body:
        return re.compile(r".*\Z")  # "/" owns everything
    out = []
    position = 0
    while position < len(body):
        if body.startswith("**/", position):
            out.append(r"(?:[^/]+/)*")
            position += 3
        elif body.startswith("**", position):
            out.append(r".*")
            position += 2
        elif body[position] == "*":
            out.append(r"[^/]*")
            position += 1
        elif body[position] == "?":
            out.append(r"[^/]")
            position += 1
        else:
            out.append(re.escape(body[position]))
            position += 1
    core = "".join(out)
    prefix = "" if anchored else r"(?:[^/]+/)*"
    # A name matches the file itself or, as a directory, everything beneath it; a trailing slash is a directory
    # only, and a last segment with a wildcard (`docs/*`) names entries at that level, not what lies below them.
    suffix = r"/.*" if directory else "" if "*" in body.rsplit("/", 1)[-1] and not body.endswith("**") else r"(?:/.*)?"
    return re.compile(prefix + core + suffix + r"\Z")


def parse_codeowners(text: str) -> List[Dict[str, Any]]:
    """Rules in file order. Section headers, comments and unsupported patterns are skipped, never guessed."""
    rules = []
    for number, raw in enumerate(str(text).splitlines(), 1):
        line = raw.split(" #", 1)[0].strip() if not raw.lstrip().startswith("#") else ""
        if not line or line.startswith("[") or line.startswith("^["):
            continue
        fields = line.split()
        matcher = _owner_pattern(fields[0])
        if matcher is None:
            continue
        rules.append({"line": number, "pattern": fields[0], "owners": fields[1:], "match": matcher})
    return rules


def owners_for(rules: Sequence[Mapping[str, Any]], path: str) -> Tuple[List[str], str]:
    """(owners, pattern) of the last rule that matches, as CODEOWNERS defines; ([], "") when none does."""
    for rule in reversed(rules):
        if rule["match"].match(path):
            return list(rule["owners"]), str(rule["pattern"])
    return [], ""


def read_codeowners(repo: Path) -> Tuple[str, List[Dict[str, Any]]]:
    """The first CODEOWNERS file in the places hosts read, parsed; ("", []) when the repository has none.

    Read only, never followed out of the checkout and never executed: a symlink
    or an oversized file is treated as absent."""
    for relative in CODEOWNERS_PATHS:
        path = Path(repo) / relative
        try:
            if path.is_symlink() or not path.is_file() or path.stat().st_size > CODEOWNERS_MAX_BYTES:
                continue
            return relative, parse_codeowners(path.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            continue
    return "", []


def _cut(rows: Sequence[Any], limit: int, name: str, omitted: Dict[str, int]) -> List[Any]:
    rows = list(rows)
    if len(rows) > limit:
        omitted[name] = len(rows) - limit
    return rows[:limit]


def impact_brief(repo: Path, graph: Mapping[str, Any], *, base: str, head: str = "HEAD", limit: int = 20,
                 depth: int = 3, commits: int = project_history.DEFAULT_COMMITS,
                 boundary_patterns: Sequence[str] = project_affected.DEFAULT_BOUNDARIES,
                 runnable_patterns: Sequence[str] = ()) -> Dict[str, Any]:
    if limit < 1 or limit > 500:
        raise GraphError("project impact --limit must be between 1 and 500")
    if depth < 0:
        raise GraphError("project impact --depth must be zero or greater")
    repo = Path(repo).resolve()
    index = Graph(graph)
    changes = project_affected.changed_entries(repo, base, head)
    paths = [row["path"] for row in changes]
    changed = set(paths) | {row["old_path"] for row in changes if row.get("old_path")}
    plan = project_affected.affected_plan(
        index, paths, changes=changes,
        test_hunks=project_affected.changed_test_hunks(repo, base, head, changes),
        repo=repo, base=base, head=head, boundary_patterns=tuple(boundary_patterns),
        runnable_patterns=tuple(runnable_patterns),
        workspace_exact=project_affected.workspace_matches_head(repo, head),
    )
    radius = blast_radius(index, paths, depth=depth, confidences=("EXTRACTED",))
    tests = set(radius["tests"])
    dependents = [path for path in radius["files"] if path not in changed and path not in tests]

    # History names what usually moves with a changed path; only the partners this range left alone are news.
    report = project_history.coupling_report(repo, graph, commits=commits, focus=sorted(changed), limit=0)
    partners: Dict[str, Dict[str, Any]] = {}
    for row in report["pairs"]:
        for own, other in ((row["a"], row["b"]), (row["b"], row["a"])):
            if own in changed and other not in changed:
                best = partners.get(other)
                if best is None or (row["degree"], row["shared_revs"]) > (best["degree"], best["shared_revs"]):
                    partners[other] = {"path": other, "with": own, "degree": row["degree"],
                                       "shared_revs": row["shared_revs"], "structural": bool(row["structural"])}
    co_change = sorted(partners.values(), key=lambda item: (-item["degree"], -item["shared_revs"], item["path"]))

    owners_file, rules = read_codeowners(repo)
    owned = []
    if owners_file:
        for path in paths:
            names, pattern = owners_for(rules, path)
            owned.append({"path": path, "owners": names, "pattern": pattern})

    omitted: Dict[str, int] = {}
    brief = {
        "schema": 1, "kind": "impact-brief", "note": NOTE,
        "base": base, "head": head, "project_graph_revision": str(graph.get("revision", "")),
        "changes": _cut(changes, limit, "changes", omitted),
        "affected": {
            "mode": plan["mode"], "reasons": _cut(plan["reasons"], limit, "affected.reasons", omitted),
            "tests": _cut(plan["tests"], limit, "affected.tests", omitted),
            "test_cases": _cut([row["id"] for row in plan["test_cases"]], limit, "affected.test_cases", omitted),
            "path_coverage": plan["path_coverage"],
            "unmatched": _cut(plan["unmatched"], limit, "affected.unmatched", omitted),
        },
        "documentation_paths": _cut(plan["documentation_paths"], limit, "documentation_paths", omitted),
        "dependents": {
            "basis": "EXTRACTED reverse dependencies, depth %d" % depth,
            "files": _cut(dependents, limit, "dependents.files", omitted),
            "depth_truncated": bool(radius["truncated"]),
        },
        "co_change": {
            "basis": "Git history, %d commit(s) read, %d bulk commit(s) skipped" % (report["commits"], report["skipped_bulk"]),
            "not_in_range": _cut(co_change, limit, "co_change.not_in_range", omitted),
        },
        "owners": {"file": owners_file, "paths": _cut(owned, limit, "owners.paths", omitted)} if owners_file else None,
        "limits": {"per_list": limit},
    }
    brief["omitted"] = omitted
    brief["truncated"] = bool(omitted)
    return brief


def brief_text(brief: Mapping[str, Any]) -> str:
    """The brief as lines a reviewer or worker prompt can carry as is."""
    lines = ["impact brief %s..%s (%s)" % (brief["base"], brief["head"], brief["note"])]
    for row in brief["changes"]:
        lines.append("changed: %s %s" % (row["status"], row["path"]))
    affected = brief["affected"]
    lines.append("tests: mode=%s path_coverage=%s" % (affected["mode"], affected["path_coverage"]))
    lines.extend("  reason: %s" % item for item in affected["reasons"])
    lines.extend("  test: %s" % item for item in affected["tests"])
    lines.extend("  test_case: %s" % item for item in affected["test_cases"])
    lines.extend("  unmatched: %s" % item for item in affected["unmatched"])
    lines.extend("documentation: %s" % item for item in brief["documentation_paths"])
    dependents = brief["dependents"]
    lines.append("dependents (%s%s):" % (dependents["basis"], ", deeper ones not followed" if dependents["depth_truncated"] else ""))
    lines.extend("  %s" % item for item in dependents["files"])
    lines.append("usually changes with these but is not in this range (%s):" % brief["co_change"]["basis"])
    lines.extend("  %s  with %s  %.1f%%  shared %d  structural=%s"
                 % (row["path"], row["with"], row["degree"], row["shared_revs"], "yes" if row["structural"] else "no")
                 for row in brief["co_change"]["not_in_range"])
    if brief["owners"]:
        lines.append("owners (%s, last matching rule):" % brief["owners"]["file"])
        lines.extend("  %s  %s" % (row["path"], " ".join(row["owners"]) or "(no owner)") for row in brief["owners"]["paths"])
    for name, count in sorted(brief["omitted"].items()):
        lines.append("omitted: %s +%d (raise --limit)" % (name, count))
    return "\n".join(lines) + "\n"
