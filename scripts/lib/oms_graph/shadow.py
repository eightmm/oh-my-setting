"""Evaluator-vs-control-plane comparison ledger (W3)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def shadow(repo: Path, *, spec_name: str = "goal-drive") -> Dict[str, Any]:
    _todo("shadow.shadow")
