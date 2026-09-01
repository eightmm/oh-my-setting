"""`exec run` step loop: tool nodes, agent nodes via adapters, gates, caching, resume (W3)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def run(repo: Path, spec: Mapping[str, Any], *, worker: str, run_id: str = "", model: str = "", reasoning_effort: str = "", max_steps: Optional[int] = None, jobs: int = 1, goal: str = "", dry_run: bool = False) -> Dict[str, Any]:
    _todo("runner.run")


def resume(repo: Path, run_id: str, *, worker: str, **options: Any) -> Dict[str, Any]:
    _todo("runner.resume")


def decide(repo: Path, run_id: str, node: str, outcome: str, *, note: str = "") -> Dict[str, Any]:
    _todo("runner.decide")
