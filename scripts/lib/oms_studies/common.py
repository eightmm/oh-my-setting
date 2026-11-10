"""Bounded inputs, content objects, and immutable records for studies."""
from __future__ import annotations

import math
import json
import os
import re
import uuid
from pathlib import Path, PurePosixPath
from typing import Any

from oms_runtime.common import (
    CoreError, MAX_STATE_BYTES, atomic_write_bytes, atomic_write_json,
    canonical_json, ensure_private_dir, file_lock, read_bytes, read_json,
    safe_id, sha256_bytes, utc_now,
)

MAX_FILE_BYTES = 32 * 1024 * 1024
MAX_ROWS = 10000
HASH = re.compile(r"[0-9a-f]{64}")


def writable() -> None:
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise CoreError("a harness child may only use read-only study actions")


def identifier(prefix: str) -> str:
    return prefix + "-" + uuid.uuid4().hex[:20]


def state(repo: Path) -> Path:
    return repo / ".oms" / "studies"


def relative(value: Any, label: str = "path") -> str:
    if not isinstance(value, str) or not value or len(value) > 500:
        raise CoreError("%s must be a nonempty repository-relative path" % label)
    parts = PurePosixPath(value).parts
    if (value.startswith(("/", "~")) or "\\" in value or ":" in value or
            any(ord(c) < 32 or ord(c) == 127 for c in value) or
            any(p in (".", "..") for p in value.split("/")) or
            "//" in value or not parts):
        raise CoreError("%s must be a normalized repository-relative path" % label)
    return value


def local(repo: Path, value: Any) -> Path:
    rel = relative(value)
    path = repo / rel
    current = repo
    for part in PurePosixPath(rel).parts:
        current = current / part
        if current.is_symlink():
            raise CoreError("symlink paths are not study inputs: %s" % rel)
    if not path.resolve().is_relative_to(repo.resolve()):
        raise CoreError("study path escapes the repository")
    return path


def finite_json(value: Any, label: str = "JSON", depth: int = 0) -> Any:
    if depth > 30:
        raise CoreError("%s is nested too deeply" % label)
    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float) and math.isfinite(value):
        return value
    if isinstance(value, list):
        for item in value:
            finite_json(item, label, depth + 1)
        return value
    if isinstance(value, dict) and all(isinstance(k, str) for k in value):
        for item in value.values():
            finite_json(item, label, depth + 1)
        return value
    raise CoreError("%s must contain only finite JSON values" % label)


def obj(value: Any, label: str) -> dict:
    if not isinstance(value, dict):
        raise CoreError("%s must be an object" % label)
    finite_json(value, label)
    return value


def number(value: Any, label: str) -> int | float:
    try:
        valid = not isinstance(value, bool) and isinstance(value, (int, float)) and math.isfinite(value)
    except OverflowError:
        valid = False
    if not valid:
        raise CoreError("%s must be a finite number" % label)
    return value


def digest(value: Any, label: str = "sha256") -> str:
    if not isinstance(value, str) or not HASH.fullmatch(value):
        raise CoreError("%s must be a SHA-256 hex digest" % label)
    return value


def put_blob(repo: Path, data: bytes) -> dict:
    writable()
    if len(data) > MAX_FILE_BYTES:
        raise CoreError("study artifact exceeds 32 MiB; use an external artifact system")
    sha = sha256_bytes(data)
    path = state(repo) / "objects" / sha
    if path.exists() or path.is_symlink():
        local(repo, path.relative_to(repo).as_posix())
        if sha256_bytes(read_bytes(path, MAX_FILE_BYTES)) != sha:
            raise CoreError("immutable study object was modified: %s" % sha)
        return {"path": path.relative_to(repo).as_posix(), "sha256": sha, "bytes": len(data)}
    with file_lock(path):
        if path.exists() or path.is_symlink():
            if sha256_bytes(read_bytes(path, MAX_FILE_BYTES)) != sha:
                raise CoreError("immutable study object was modified: %s" % sha)
        else:
            atomic_write_bytes(path, data, mode=0o400)
    return {"path": path.relative_to(repo).as_posix(), "sha256": sha, "bytes": len(data)}


def snapshot(repo: Path, raw: str) -> dict:
    path = local(repo, raw)
    desc = put_blob(repo, read_bytes(path, MAX_FILE_BYTES))
    value = dict(desc, source=relative(raw), executable=bool(path.stat().st_mode & 0o111))
    suffix = path.suffix.lstrip(".").lower()
    if re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,19}", suffix):
        value["format"] = suffix
    return value


def blob(repo: Path, desc: dict) -> bytes:
    obj(desc, "artifact")
    want = digest(desc.get("sha256"))
    data = read_bytes(local(repo, desc.get("path")), MAX_FILE_BYTES)
    if sha256_bytes(data) != want:
        raise CoreError("artifact hash mismatch: %s" % desc.get("path"))
    return data


def record_path(repo: Path, kind: str, name: str) -> Path:
    return state(repo) / kind / (safe_id(name, kind + " id") + ".json")


def store(repo: Path, kind: str, name: str, value: dict) -> dict:
    writable()
    path = record_path(repo, kind, name)
    data = dict(value)
    data.setdefault("schema", 1)
    data.setdefault("created_at", utc_now())
    data.pop("content_sha256", None)
    data["content_sha256"] = sha256_bytes(canonical_json(data))
    if len(canonical_json(data)) > MAX_STATE_BYTES:
        raise CoreError("study record exceeds the supported 8 MiB bound")
    with file_lock(path):
        if path.exists() or path.is_symlink():
            raise CoreError("immutable %s id already exists: %s" % (kind, name))
        atomic_write_json(path, data, mode=0o400)
    return data


def checked(value: Any) -> dict:
    value = obj(value, "study record")
    if "content_sha256" in value:
        body = {k: v for k, v in value.items() if k != "content_sha256"}
        if sha256_bytes(canonical_json(body)) != digest(value["content_sha256"]):
            raise CoreError("immutable study record hash mismatch")
    return value


def load(repo: Path, kind: str, name: str) -> dict:
    path = record_path(repo, kind, name)
    local(repo, path.relative_to(repo).as_posix())
    value = read_json(path)
    if value is None:
        raise CoreError("unknown %s id: %s" % (kind, name))
    value = checked(value)
    if "content_sha256" not in value:
        raise CoreError("immutable study record is missing its digest")
    return value


def records(repo: Path, kind: str) -> list[dict]:
    root = state(repo) / kind
    local(repo, root.relative_to(repo).as_posix())
    if root.is_symlink():
        raise CoreError("study record directory must not be a symlink")
    paths = sorted(root.glob("*.json")) if root.exists() else []
    if len(paths) > MAX_ROWS:
        raise CoreError("too many study records; archive the study")
    return [load(repo, kind, p.stem) for p in paths]


def directions(raw: Any) -> dict:
    value = obj(raw, "metric_directions")
    if not value or len(value) > 50:
        raise CoreError("metric_directions needs 1-50 metrics")
    for name, direction in value.items():
        safe_id(name, "metric name")
        if direction not in ("min", "max"):
            raise CoreError("metric directions must be min or max")
    return dict(value)


def metrics(raw: Any, expected: dict | None = None) -> dict:
    value = obj(raw, "metrics")
    if len(value) > 100:
        raise CoreError("too many metrics")
    for key, val in value.items():
        safe_id(key, "metric name")
        number(val, "metric %s" % key)
    if expected and set(expected) - set(value):
        raise CoreError("evaluator omitted required metric: %s" % sorted(set(expected) - set(value))[0])
    return dict(value)


def results(raw: Any) -> dict:
    value = checked(raw)
    if type(value.get("schema")) is not int or value["schema"] != 1 or value.get("kind") != "oms-study-results":
        raise CoreError("expected schema 1 oms-study-results")
    directions(value.get("metric_directions"))
    for field in ("suite_sha256", "base_sha256", "evaluator_sha256", "source_sha256"):
        if value.get(field) is not None:
            digest(value[field], field)
    rows = value.get("rows")
    if not isinstance(rows, list) or len(rows) > MAX_ROWS:
        raise CoreError("results rows must be a list of at most %d rows" % MAX_ROWS)
    seen = set()
    for row in rows:
        obj(row, "result row")
        if "params" not in row or "artifacts" not in row:
            raise CoreError("result rows require params and artifacts")
        if any(not isinstance(row.get(field), str) for field in ("run_id", "case_id", "variant")):
            raise CoreError("run_id, case_id, and variant must be strings")
        run = safe_id(row.get("run_id"), "run_id")
        if run in seen:
            raise CoreError("duplicate run_id: %s" % run)
        seen.add(run)
        safe_id(row.get("case_id"), "case_id")
        safe_id(row.get("variant"), "variant")
        if type(row.get("repeat")) is not int or row["repeat"] < 0:
            raise CoreError("repeat must be a nonnegative integer")
        if row.get("status") not in ("complete", "failed", "interrupted"):
            raise CoreError("result status must be complete, failed, or interrupted")
        metrics(row.get("metrics"), value["metric_directions"] if row["status"] == "complete" else None)
        for field in ("suite_sha256", "base_sha256", "evaluator_sha256", "input_sha256", "prediction_sha256"):
            if row.get(field) is not None:
                digest(row[field], field)
        params = obj(row.get("params", {}), "params")
        if any(isinstance(v, (dict, list)) for v in params.values()):
            raise CoreError("result params must contain scalar JSON values")
        artifacts = row.get("artifacts", [])
        if not isinstance(artifacts, list) or len(artifacts) > 200:
            raise CoreError("artifacts must be a list of at most 200 descriptors")
        for artifact in artifacts:
            obj(artifact, "artifact")
            relative(artifact.get("path"))
            if not isinstance(artifact.get("type"), str) or not isinstance(artifact.get("label"), str):
                raise CoreError("artifacts require type and label strings")
            if "sha256" in artifact:
                digest(artifact["sha256"])
            if "format" in artifact:
                format_name(artifact["format"])
    return value


def read_results(repo: Path, raw: str) -> tuple[dict, str]:
    path = local(repo, raw)
    data = read_bytes(path)
    try:
        value = json.loads(data.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as exc:
        raise CoreError("invalid study results JSON: %s" % exc)
    return results(value), sha256_bytes(data)


def artifact(repo: Path, data: bytes, kind: str, label: str, **metadata: Any) -> dict:
    if kind in ("input", "metrics"):
        metadata.setdefault("format", "json")
    return dict(put_blob(repo, data), type=kind, label=label, **metadata)


def format_name(value: Any) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,19}", value):
        raise CoreError("artifact format must be a lowercase format label of 1-20 characters")
    return value


def workdir(repo: Path, category: str, name: str) -> Path:
    writable()
    path = state(repo) / "work" / category / safe_id(name)
    if path.exists() or path.is_symlink():
        raise CoreError("study work directory already exists: %s" % name)
    return ensure_private_dir(path)
