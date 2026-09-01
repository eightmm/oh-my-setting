"""File discovery, safety filters, per-file extraction (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)

DEFAULT_EXCLUDES = (".git/", ".oms/", "node_modules/", "vendor/", "dist/", "build/", "target/", "__pycache__/")
SECRET_NAME_GLOBS = (".env", ".env.*", "*.pem", "*.key", "id_rsa*", "*.p12", "*.pfx")


def discover_files(repo: Path, *, include: Sequence[str] = (), exclude: Sequence[str] = (), max_bytes: int = 2 * 1024 * 1024) -> Dict[str, Any]:
    """{"files": [relpath...], "skipped": [{"path","reason"}]} — no symlinks, no binaries, no secrets."""
    _todo("project.extract.discover_files")


def extract_file(repo: Path, relpath: str) -> Dict[str, Any]:
    """{"path","sha256","bytes","parser","parser_version","nodes","edges","refs"} for one file."""
    _todo("project.extract.extract_file")
