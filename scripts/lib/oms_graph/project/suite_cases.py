"""Changed-case selection for a test suite the Project Graph does not index.

`tests/scripts-smoke.sh` is larger than the graph's per-file size bound, so no
file node exists for it and a change to it was always "unmatched" (a full
selection). A suite whose test functions are top-level `test_NAME() {` blocks
can still be narrowed from the diff alone: each changed line either sits inside
one test function (that case runs), sits inside a helper or in top-level code
(the whole suite file runs), or is blank. Anything this reader cannot prove
falls back to the suite file, never to the global full selection.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List, Mapping, Optional, Sequence, Set, Tuple

# Suites whose cases `check.sh` can run one by one (`run-smoke-shard.sh --only`).
SELECTABLE_SUITES = ("tests/scripts-smoke.sh", "tests/autonomy-plan-run-smoke.sh")

_DEFINITION = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s*\(\)\s*\{(.*)$")
_HEREDOC = re.compile(r"(?<!<)<<(-?)\s*\\?(?:'([^']+)'|\"([^\"]+)\"|([A-Za-z_][A-Za-z0-9_]*))")
_TEST_START = re.compile(r"^test_[A-Za-z0-9_]+\(\) \{")
_CLOSE = re.compile(r"^}\s*(?:#.*)?$")
_SHARED_COMMENT = re.compile(r"^\s*#\s*(?:!|shellcheck|SMOKE_TEST_CALLS|smoke-weight)")


def _heredocs(line: str) -> List[Tuple[str, bool]]:
    return [(m.group(2) or m.group(3) or m.group(4), m.group(1) == "-") for m in _HEREDOC.finditer(line)]


def _top_level_functions(text: str) -> Optional[Dict[str, Tuple[int, int, int]]]:
    """name -> (first line incl. comments above, definition line, closing line); None if unproven."""
    lines = text.split("\n")
    found: Dict[str, Tuple[int, int, int]] = {}
    pending: List[Tuple[str, bool]] = []
    name, start = "", 0
    for number, raw in enumerate((line.rstrip("\r") for line in lines), 1):
        if pending:
            word, strip = pending[0]
            if (raw.lstrip("\t") if strip else raw) == word:
                pending.pop(0)
            continue
        if not name:
            match = _DEFINITION.match(raw)
            if match:
                body = match.group(2).rstrip()
                if body.endswith("}") and not _heredocs(raw):
                    if match.group(1) in found:
                        return None
                    found[match.group(1)] = (0, number, number)
                    continue
                name, start = match.group(1), number
        elif _CLOSE.match(raw):
            if name in found:
                return None
            found[name] = (0, start, number)
            name = ""
            continue
        pending.extend(_heredocs(raw))
    if name or pending:
        return None
    # Pull the comment block directly above a definition into its function:
    # a "# smoke-weight:" line belongs to the case below it.
    covered: Dict[str, Tuple[int, int, int]] = {}
    for key, (_, definition, end) in found.items():
        first = definition
        while first > 1 and lines[first - 2].lstrip().startswith("#") and not lines[first - 2].startswith("#!"):
            first -= 1
        covered[key] = (first, definition, end)
    ordered = sorted(covered.values())
    for before, after in zip(ordered, ordered[1:]):
        if after[0] <= before[2]:
            return None
    # The runner's own manifest is "every col-0 test_NAME() {": the two views
    # must agree, or a heredoc/quote the reader misjudged hid or merged a case.
    manifest = [raw.split("(", 1)[0] for raw in lines if _TEST_START.match(raw)]
    mine = sorted((v[1], k) for k, v in covered.items() if k.startswith("test_"))
    if [k for _, k in mine] != manifest:
        return None
    return covered


def _owner(ranges: Mapping[str, Tuple[int, int, int]], line: int) -> Optional[str]:
    for name, (first, _, end) in ranges.items():
        if first <= line <= end:
            return name
    return None


def changed_suite_cases(
    old_text: str,
    new_text: str,
    hunks: Sequence[Mapping[str, Any]],
) -> Optional[List[str]]:
    """Names of the test functions to run, [] when none, None for "run the suite file"."""
    old_ranges = _top_level_functions(old_text)
    new_ranges = _top_level_functions(new_text)
    if not hunks or old_ranges is None or new_ranges is None:
        return None
    selected: Set[str] = set()
    for hunk in hunks:
        for side, ranges in (("old_lines", old_ranges), ("new_lines", new_ranges)):
            for number, line in hunk.get(side, []):
                owner = _owner(ranges, int(number))
                if owner is None:
                    if line.strip() == "" or (line.lstrip().startswith("#") and not _SHARED_COMMENT.match(line)):
                        continue
                    return None
                if not owner.startswith("test_"):
                    return None
                if owner in new_ranges:
                    selected.add(owner)
    return sorted(selected)


def case_rows(path: str, names: Sequence[str]) -> List[Dict[str, str]]:
    return [{"id": "symbol:%s::%s" % (path, name), "language": "shell", "name": name, "path": path}
            for name in names]
