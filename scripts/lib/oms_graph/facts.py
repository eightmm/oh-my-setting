"""Fact collectors: plan, receipts, git -> flat dict (W1)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def collect_facts(repo: Path, *, include: Sequence[str] = ("git", "plan", "receipts")) -> Dict[str, Any]:
    """Flat fact dict with the keys listed in docs/GRAPH-ENGINEERING.md."""
    _todo("facts.collect_facts")


def git_facts(repo: Path) -> Dict[str, Any]:
    _todo("facts.git_facts")


def plan_facts(repo: Path) -> Dict[str, Any]:
    """agent-plan status --json plus show --id per task; never writes."""
    _todo("facts.plan_facts")


def receipt_facts(repo: Path, *, head: str = "") -> Dict[str, Any]:
    """From oms_runtime.evidence.artifact_rows and .oms/plan/progress.jsonl."""
    _todo("facts.receipt_facts")
