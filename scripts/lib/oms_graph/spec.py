"""Execution GraphSpec: load, normalize, digest, bundled specs (W1)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def bundled_dir() -> Path:
    """config/graphs under the installation root."""
    from oms_runtime.common import install_root
    return install_root() / "config" / "graphs"


def bundled_names() -> List[str]:
    return sorted(path.stem for path in bundled_dir().glob("*.json"))


def load_spec(source: Any) -> Dict[str, Any]:
    """Accept a path, a bundled name, JSON text, or a mapping; return a normalized spec."""
    _todo("spec.load_spec")


def normalize_spec(raw: Mapping[str, Any]) -> Dict[str, Any]:
    """Fill defaults per docs/GRAPH-ENGINEERING.md; raise GraphError on shape errors."""
    _todo("spec.normalize_spec")


def spec_digest(spec: Mapping[str, Any]) -> str:
    """sha256 of the canonical JSON of a normalized spec."""
    _todo("spec.spec_digest")
