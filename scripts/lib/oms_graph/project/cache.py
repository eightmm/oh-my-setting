"""Content-addressed extraction cache under .oms/project-graph/cache (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import atomic_write_json, ensure_private_dir, read_json, sha256_bytes


def cache_key(relpath: str, sha256: str, parser_version: int, schema: int) -> str:
    return sha256_bytes(("%s\n%s\n%d\n%d" % (relpath, sha256, parser_version, schema)).encode("utf-8"))


def load_cached(state_dir: Path, key: str) -> Optional[Dict[str, Any]]:
    value = read_json(state_dir / "cache" / (key + ".json"), None)
    return value if isinstance(value, dict) else None


def store(state_dir: Path, key: str, payload: Mapping[str, Any]) -> Path:
    directory = ensure_private_dir(state_dir / "cache")
    path = directory / (key + ".json")
    atomic_write_json(path, dict(payload))
    return path


def prune(state_dir: Path, keep: Sequence[str]) -> int:
    directory = state_dir / "cache"
    if not directory.is_dir() or directory.is_symlink():
        return 0
    wanted = set(keep)
    removed = 0
    for path in directory.glob("*.json"):
        if path.stem not in wanted and path.is_file() and not path.is_symlink():
            path.unlink()
            removed += 1
    return removed
