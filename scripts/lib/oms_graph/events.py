"""Run store: frozen spec, append-only events, projection, resume (W1)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)

import re

RUN_ID_RE = re.compile(r"^run-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$")


def runs_root(repo: Path) -> Path:
    return repo / ".oms" / "graph" / "runs"


def run_dir(repo: Path, run_id: str) -> Path:
    if not RUN_ID_RE.fullmatch(str(run_id or "")):
        raise GraphError("invalid run id: %s" % run_id)
    return runs_root(repo) / run_id


def new_run_id() -> str:
    _todo("events.new_run_id")


def start_run(repo: Path, spec: Mapping[str, Any], *, run_id: str = "", options: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    _todo("events.start_run")


def append_event(repo: Path, run_id: str, event: str, **fields: Any) -> Dict[str, Any]:
    _todo("events.append_event")


def read_events(repo: Path, run_id: str) -> List[Dict[str, Any]]:
    _todo("events.read_events")


def load_run_spec(repo: Path, run_id: str) -> Dict[str, Any]:
    _todo("events.load_run_spec")


def project(events: Sequence[Mapping[str, Any]], spec: Mapping[str, Any]) -> Dict[str, Any]:
    """Pure, idempotent fold of events into evaluator state."""
    _todo("events.project")


def write_projection(repo: Path, run_id: str, projection: Mapping[str, Any]) -> Path:
    _todo("events.write_projection")


def list_runs(repo: Path) -> List[Dict[str, Any]]:
    _todo("events.list_runs")


def latest_run_id(repo: Path) -> str:
    _todo("events.latest_run_id")
