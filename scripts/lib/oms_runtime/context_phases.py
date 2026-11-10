"""Phase -> context composition rules, read from config/context-phases.json.

A phase used to be a label on the manifest. The table makes it a deterministic
rule: how far a kind of candidate moves in the ordering, and how large one
non-required source may grow. Targets, required files and the project
contract are never reordered by a phase, and the table is data, not code: it
is validated strictly and a manifest records the digest of the table it was
built under, so two packs are comparable only when that digest matches.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, Mapping

from .common import CoreError, sha256_bytes

PHASES = ("orientation", "implementation", "verification", "review", "research")
# What a rule may move. Direct targets, required context and the project contract are not listed on purpose.
CLASSES = ("task-packet", "pins", "memory-summary", "work-journal", "related", "related-test", "explicit")
DEFAULT_LAYER_CLASSES = {"active task packet": "task-packet", "pinned project facts": "pins",
                         "compacted project memory": "memory-summary", "current work journal": "work-journal"}
# A phase may lift a candidate up to, never over, the lowest protected priority (direct target, 120).
PRIORITY_CEILING = 119
DIVISOR_RANGE = (2, 16)
DELTA_RANGE = (-60, 60)
TABLE_PATH = Path(__file__).resolve().parents[3] / "config" / "context-phases.json"


def load_rules(path: Path = TABLE_PATH) -> Dict[str, Any]:
    """The validated table plus the sha256 of its bytes. Anything unexpected is refused, never guessed."""
    try:
        raw = Path(path).read_bytes()
        table = json.loads(raw.decode("utf-8"))
    except (OSError, ValueError) as exc:
        raise CoreError("context phase rules are unreadable (%s): %s" % (path.name, exc))
    phases = table.get("phases") if isinstance(table, dict) else None
    if not isinstance(table, dict) or table.get("schema") != 1 or not isinstance(phases, dict) \
            or set(table) - {"schema", "note", "phases"} or set(phases) != set(PHASES):
        raise CoreError("context phase rules must be schema 1 with exactly these phases: %s" % ", ".join(PHASES))
    for name, rule in phases.items():
        if not isinstance(rule, dict) or set(rule) - {"nonrequired_divisor", "priority"}:
            raise CoreError("context phase rule %s has an unknown key" % name)
        divisor = rule.get("nonrequired_divisor", 3)
        if type(divisor) is not int or not DIVISOR_RANGE[0] <= divisor <= DIVISOR_RANGE[1]:
            raise CoreError("context phase rule %s: nonrequired_divisor must be an integer from %d to %d"
                            % ((name,) + DIVISOR_RANGE))
        deltas = rule.get("priority", {})
        if not isinstance(deltas, dict) or set(deltas) - set(CLASSES) or any(
                type(value) is not int or not DELTA_RANGE[0] <= value <= DELTA_RANGE[1] for value in deltas.values()):
            raise CoreError("context phase rule %s: priority maps %s to integers from %d to %d"
                            % ((name, "/".join(CLASSES)) + DELTA_RANGE))
    return {"sha256": sha256_bytes(raw), "phases": phases}


def candidate_class(reason: str, path_label: str, *, explicit: bool = False, related: bool = False) -> str:
    """Which movable class a candidate is, "" when a phase may not move it."""
    if explicit:
        return "explicit"
    if related:
        name = path_label.rsplit("/", 1)[-1]
        test = (reason.endswith("related test") or path_label.startswith(("tests/", "test/")) or "/tests/" in path_label
                or name.startswith("test_") or name.endswith(("_test.py", "-smoke.sh", "_test.sh")))
        return "related-test" if test else "related"
    return DEFAULT_LAYER_CLASSES.get(reason, "")


def adjusted(priority: int, kind: str, rule: Mapping[str, Any]) -> int:
    delta = int(rule.get("priority", {}).get(kind, 0)) if kind else 0
    if not delta:
        return priority
    return max(1, min(PRIORITY_CEILING, priority + delta))
