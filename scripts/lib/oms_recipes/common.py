"""Bounded validation and private typed state for the recipe feature family."""

from __future__ import annotations

import datetime as dt
import json
import os
from pathlib import Path, PurePosixPath
import re
from typing import Any

from oms_runtime.common import (
    MAX_JSONL_BYTES, CoreError, SECRET_NAME_RE, SECRET_VALUE_RE, append_jsonl, atomic_write_bytes,
    canonical_json, ensure_private_dir, file_lock, read_bytes, read_json,
    sha256_bytes, utc_now,
)

MAX_DEFINITION_BYTES = 256 * 1024
MAX_RESOURCES = 64
MAX_RESOURCE_BYTES = 4 * 1024 * 1024
MAX_PACK_BYTES = 16 * 1024 * 1024
MAX_RECORDS = 1024
MAX_STEPS = 64
MAX_ATTEMPTS = 256
NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_-]{0,63}$")
ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$")
HEX = re.compile(r"^[a-f0-9]{64}$")


def identifier(value: Any, label: str = "id", *, name: bool = False) -> str:
    if not isinstance(value, str) or not (NAME if name else ID).fullmatch(value):
        raise CoreError("invalid " + label)
    return value


def object_fields(value: Any, allowed: set[str], required: set[str], label: str) -> dict:
    if not isinstance(value, dict) or not required <= value.keys() or value.keys() - allowed:
        raise CoreError(label + " has missing, unknown or invalid fields")
    return value


def text(value: Any, label: str, maximum: int = 4096, *, empty: bool = False) -> str:
    if not isinstance(value, str) or len(value.encode("utf-8")) > maximum:
        raise CoreError(label + " must be bounded text")
    if (not empty and not value) or any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise CoreError(label + " contains empty or control-character text")
    if SECRET_VALUE_RE.search(value):
        raise CoreError(label + " contains sensitive-looking material")
    return value


def bounded_int(value: Any, label: str, low: int, high: int) -> int:
    if type(value) is not int or not low <= value <= high:
        raise CoreError("%s must be an integer in %d..%d" % (label, low, high))
    return value


def check_secrets(value: Any) -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            if SECRET_NAME_RE.search(str(key)):
                raise CoreError("definition contains a sensitive-looking field name")
            check_secrets(item)
    elif isinstance(value, list):
        for item in value:
            check_secrets(item)
    elif isinstance(value, str) and SECRET_VALUE_RE.search(value):
        raise CoreError("definition contains sensitive-looking material")


def digest(value: Any) -> str:
    return sha256_bytes(canonical_json(value))


def load_definition(path: Path) -> dict:
    raw = read_bytes(path, MAX_DEFINITION_BYTES)

    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result

    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=unique,
                           parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite number")))
    except (UnicodeError, ValueError, RecursionError):
        raise CoreError("definition must contain bounded UTF-8 JSON")
    if not isinstance(value, dict):
        raise CoreError("definition must be a JSON object")
    try:
        check_secrets(value)
    except RecursionError:
        raise CoreError("definition nesting is too deep")
    return value


def relative(value: Any, label: str = "path", *, dot: bool = False) -> str:
    value = text(value, label, 1024)
    parts = value.split("/")
    if dot and value == ".":
        return value
    if value.startswith(("/", "~")) or "\\" in value or ":" in value or any(p in ("", ".", "..") for p in parts):
        raise CoreError(label + " must be a normalized relative path")
    return PurePosixPath(value).as_posix()


def under(root: Path, value: str, *, dot: bool = False) -> Path:
    value = relative(value, dot=dot)
    root = Path(root)
    if root.is_symlink():
        raise CoreError("root must not be a symlink")
    path = root
    for part in (() if value == "." else value.split("/")):
        path = path / part
        if path.is_symlink():
            raise CoreError("path contains a symlink: " + value)
    if not path.resolve(strict=False).is_relative_to(root.resolve()):
        raise CoreError("path escapes its root")
    return path


def state_root(repo: Path, *, create: bool = False) -> Path:
    root = under(repo, ".oms/recipes")
    if create:
        ensure_private_dir(root)
        ignore = under(repo, ".oms/.gitignore")
        with file_lock(ignore):
            if not ignore.exists():
                atomic_write_bytes(ignore, b"*\n")
    return root


def state_path(repo: Path, group: str, key: str) -> Path:
    return under(state_root(repo), group + "/" + identifier(key) + ".json")


def records(repo: Path, group: str) -> list[dict]:
    folder = under(state_root(repo), relative(group))
    if not folder.exists():
        return []
    result = []
    size = 0
    with os.scandir(folder) as entries:
        for count, entry in enumerate(entries, 1):
            if count > MAX_RECORDS:
                raise CoreError("recipe state scan exceeds %d records" % MAX_RECORDS)
            if entry.name.endswith(".json"):
                if not entry.is_file(follow_symlinks=False):
                    raise CoreError("state records must be regular non-symlink files")
                size += entry.stat(follow_symlinks=False).st_size
                if size > MAX_JSONL_BYTES:
                    raise CoreError("recipe collection scan exceeds 32 MiB")
                value = read_json(Path(entry.path))
                if not isinstance(value, dict):
                    raise CoreError("invalid recipe state record")
                result.append(value)
    return sorted(result, key=lambda row: row.get("id", ""))


def history(repo: Path, kind: str, **fields: Any) -> None:
    path = under(state_root(repo, create=True), "history.jsonl")
    row = dict(schema=1, at=utc_now(), kind=kind, **fields)
    if (path.stat().st_size if path.exists() else 0) + len(canonical_json(row)) + 1 > MAX_JSONL_BYTES:
        raise CoreError("recipe history capacity reached")
    append_jsonl(path, row)


def timestamp(value: Any) -> dt.datetime:
    value = text(value, "UTC timestamp", 32)
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", value):
        raise CoreError("timestamps must use YYYY-MM-DDTHH:MM:SSZ")
    try:
        return dt.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
    except ValueError:
        raise CoreError("invalid UTC timestamp")


def child() -> bool:
    return os.environ.get("OMS_HARNESS_CHILD", "0") not in ("", "0") or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") not in ("", "0")
