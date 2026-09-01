"""Content-addressed extraction cache under .oms/project-graph/cache (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def cache_key(relpath: str, sha256: str, parser_version: int, schema: int) -> str:
    _todo("project.cache.cache_key")


def load_cached(state_dir: Path, key: str) -> Optional[Dict[str, Any]]:
    _todo("project.cache.load_cached")


def store(state_dir: Path, key: str, payload: Mapping[str, Any]) -> Path:
    _todo("project.cache.store")


def prune(state_dir: Path, keep: Sequence[str]) -> int:
    _todo("project.cache.prune")
