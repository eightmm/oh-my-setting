"""Changed paths -> transitive dependents (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)

DEFAULT_RELATIONS = ("imports", "calls", "references", "tests", "uses", "depends_on")


def changed_paths(repo: Path, *, base: str = "") -> Dict[str, List[str]]:
    """{"changed": [...], "untracked": [...]} from git; never writes."""
    _todo("project.blast.changed_paths")


def blast_radius(graph: Any, paths: Sequence[str], *, depth: int = 3, relations: Sequence[str] = DEFAULT_RELATIONS) -> Dict[str, Any]:
    """{"seeds","dependents":[{"id","distance","via"}],"files","tests"}."""
    _todo("project.blast.blast_radius")
