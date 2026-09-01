"""Eligibility and write-scope conflicts (W3)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def scopes_overlap(a: Sequence[str], b: Sequence[str]) -> bool:
    _todo("scheduler.scopes_overlap")


def eligible(spec: Mapping[str, Any], state: Mapping[str, Any], facts: Mapping[str, Any], *, route: Mapping[str, Any], task_scopes: Mapping[str, Mapping[str, Sequence[str]]], capacity: int = 1, active: Sequence[str] = ()) -> Dict[str, Any]:
    _todo("scheduler.eligible")
