"""Agent node <-> agent-plan/plan-run adapter (W3). Never calls land/finish/claim/review."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def plan_status(repo: Path) -> Dict[str, Any]:
    _todo("adapters.plan.plan_status")


def task_view(repo: Path, task_id: str) -> Dict[str, Any]:
    _todo("adapters.plan.task_view")


def scope_of(task: Mapping[str, Any]) -> Dict[str, List[str]]:
    _todo("adapters.plan.scope_of")


def outcome_from_task(node_spec: Mapping[str, Any], task: Mapping[str, Any], facts: Mapping[str, Any]) -> Tuple[str, List[str]]:
    _todo("adapters.plan.outcome_from_task")


def build_command(repo: Path, node_spec: Mapping[str, Any], *, provider: str, model: str = "", reasoning_effort: str = "", repair: int = 0, dry_run: bool = False) -> List[str]:
    _todo("adapters.plan.build_command")


def execute(repo: Path, node_spec: Mapping[str, Any], *, provider: str, model: str = "", reasoning_effort: str = "", repair: int = 0, timeout: int = 2700, dry_run: bool = False) -> Dict[str, Any]:
    _todo("adapters.plan.execute")
