"""Deterministic GraphSpec validator (W1)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)

ERROR_CODES = ("invalid_schema", "duplicate_node", "unknown_endpoint", "missing_entry", "unreachable_node", "missing_terminal", "terminal_outgoing_edge", "invalid_outcome", "invalid_repeat_edge", "unbounded_cycle", "unknown_subgraph", "recursive_subgraph", "invalid_fact_reference", "invalid_plan_task_reference", "ambiguous_routes", "invalid_effect", "invalid_kind", "invalid_join", "invalid_command", "invalid_budget", "invalid_gate")
WARNING_CODES = ("unrouted_outcome", "dead_end")


def validate_spec(spec: Mapping[str, Any], *, plan_tasks: Optional[Sequence[str]] = None) -> Dict[str, Any]:
    """Return {"ok": bool, "errors": [{"code","where","message"}], "warnings": [...]}."""
    _todo("validate.validate_spec")
