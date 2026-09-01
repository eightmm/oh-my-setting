"""Fact predicate grammar shared by validation, routing, and scheduling.

Grammar (docs/GRAPH-ENGINEERING.md): ``key`` (truthy), ``!key`` (falsy or
missing), ``key=value``, ``key!=value``. Values compare as strings; booleans
render as ``true``/``false``. Pure functions only.
"""

from __future__ import annotations

import re
from typing import Any, Dict, Mapping, Sequence, Tuple

from .errors import GraphError

KEY_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.:-]{0,199}$")


def parse(text: str) -> Tuple[str, str, str]:
    """Return (key, operator, value); operator is one of truthy/falsy/eq/ne."""
    raw = str(text or "").strip()
    if not raw:
        raise GraphError("empty fact predicate")
    if "!=" in raw:
        key, value = raw.split("!=", 1)
        op = "ne"
    elif "=" in raw:
        key, value = raw.split("=", 1)
        op = "eq"
    elif raw.startswith("!"):
        key, value, op = raw[1:], "", "falsy"
    else:
        key, value, op = raw, "", "truthy"
    key = key.strip()
    if not KEY_RE.fullmatch(key):
        raise GraphError("invalid fact key in predicate: %s" % raw)
    return key, op, value.strip()


def render_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return ""
    return str(value)


def holds(predicate: str, facts: Mapping[str, Any]) -> bool:
    key, op, expected = parse(predicate)
    present = key in facts
    value = facts.get(key)
    if op == "truthy":
        return present and bool(value)
    if op == "falsy":
        return not present or not bool(value)
    if op == "eq":
        return present and render_value(value) == expected
    return not present or render_value(value) != expected


def missing(predicates: Sequence[str], facts: Mapping[str, Any]) -> list:
    """Predicates that do not hold, in declaration order."""
    return [item for item in predicates if not holds(item, facts)]


def validate_all(predicates: Sequence[str]) -> Dict[str, str]:
    """Return {predicate: error} for every malformed predicate."""
    problems: Dict[str, str] = {}
    for item in predicates:
        try:
            parse(item)
        except GraphError as exc:
            problems[str(item)] = str(exc)
    return problems
