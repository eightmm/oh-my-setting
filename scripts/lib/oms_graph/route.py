"""Pure route evaluator and fixture runner (W1)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def effective_outcome(node_spec: Mapping[str, Any], claimed: str, facts: Mapping[str, Any]) -> Tuple[str, List[str]]:
    """A claimed completed/approved stands only when every proof predicate holds."""
    _todo("route.effective_outcome")


def state_from_outcomes(spec: Mapping[str, Any], outcomes: Mapping[str, str], *, gates: Optional[Mapping[str, str]] = None, repeats: Optional[Mapping[str, int]] = None) -> Dict[str, Any]:
    """Build an evaluator state from recorded outcomes (fixtures, tests)."""
    _todo("route.state_from_outcomes")


def evaluate(spec: Mapping[str, Any], state: Mapping[str, Any], facts: Mapping[str, Any], *, authority: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    """Pure: no subprocess, no disk. Output shape in docs/GRAPH-ENGINEERING.md."""
    _todo("route.evaluate")


def run_fixture(fixture: Mapping[str, Any]) -> Tuple[bool, Dict[str, Any]]:
    _todo("route.run_fixture")
