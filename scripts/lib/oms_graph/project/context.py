"""Task-specific context pack over the project graph (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def context_pack(repo: Path, graph: Any, *, task: str, max_files: int = 12, max_nodes: int = 40, depth: int = 2, base: str = "", state: Optional[Path] = None) -> Dict[str, Any]:
    """Write .oms/project-graph/context/<digest>.json and return the pack."""
    _todo("project.context.context_pack")


def compile_bundle(repo: Path, pack: Mapping[str, Any], *, max_bytes: int = 64 * 1024) -> Dict[str, Any]:
    """Reuse oms_runtime.context.plan_context(explicit=...) for the pack's files."""
    _todo("project.context.compile_bundle")
