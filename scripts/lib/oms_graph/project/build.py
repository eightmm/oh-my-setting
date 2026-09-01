"""Incremental build, cross-file resolution, freshness check (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def state_dir(repo: Path, override: Optional[Path] = None) -> Path:
    return Path(override) if override else repo / ".oms" / "project-graph"


def build(repo: Path, *, state: Optional[Path] = None, include: Sequence[str] = (), exclude: Sequence[str] = (), max_bytes: int = 2 * 1024 * 1024, force: bool = False) -> Dict[str, Any]:
    """Write graph.json (no timestamps) and manifest.json; return the manifest summary."""
    _todo("project.build.build")


def check(repo: Path, *, state: Optional[Path] = None) -> Dict[str, Any]:
    """{"present","fresh","revision","stale":[...],"missing":[...],"new":[...]} from working-tree bytes."""
    _todo("project.build.check")


def load_graph(repo: Path, *, state: Optional[Path] = None) -> Dict[str, Any]:
    _todo("project.build.load_graph")


def resolve(extractions: Sequence[Mapping[str, Any]]) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    """Turn per-file extractions (with unresolved refs) into sorted nodes and edges with confidence."""
    _todo("project.build.resolve")
