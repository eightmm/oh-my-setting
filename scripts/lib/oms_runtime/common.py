"""Shared standard-library primitives for OMS runtime projections and receipts."""

from __future__ import annotations

import contextlib
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, Iterator, List, Mapping, Optional, Sequence

MAX_STATE_BYTES = 8 * 1024 * 1024
MAX_JSONL_ROWS = 250_000
MAX_JSONL_BYTES = 32 * 1024 * 1024
MAX_JSONL_LINE_BYTES = 1024 * 1024
SAFE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$")
SECRET_NAME_RE = re.compile(r"(?i)(?:token|secret|pass" r"word|api[_-]?key|credential|private[_-]?key|auth[_-]?key)")
SECRET_VALUE_RE = re.compile(
    r"(?i)(?:api[_-]?key|access[_-]?token|client[_-]?secret|pass" r"word|private[_-]?key)\s*[:=]|"
    r"-----BE" r"GIN [A-Z ]+PRIVATE " r"KEY-----|"
    r"\b(?:ghp|github_pat|sk)-[A-Za-z0-9_-]{12,}\b|"
    r"\bBearer\s+[A-Za-z0-9._~+/-]{12,}={0,2}\b"
)
ABSOLUTE_PATH_RE = re.compile(r"(?<![A-Za-z0-9_.:/-])(?:/(?!/)(?:[^\s/]+/)+[^\s]+|/Us" r"ers/[^\s]+|/ho" r"me/[^\s]+|[A-Za-z]:[\\/][^\s]+)")


class CoreError(Exception):
    def __init__(self, message: str, exit_code: int = 2) -> None:
        super().__init__(message)
        self.exit_code = exit_code


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def canonical_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def pretty_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_text(value: str) -> str:
    return sha256_bytes(value.encode("utf-8"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def bounded_line(value: Any, limit: int = 500) -> str:
    text = " ".join(str(value or "").replace("\r", " ").replace("\n", " ").replace("\t", " ").split())
    return text if len(text) <= limit else text[: max(0, limit - 1)] + "…"


def safe_id(value: Any, label: str = "id", *, optional: bool = False) -> str:
    text = str(value or "")
    if not text and optional:
        return ""
    if not SAFE_ID_RE.fullmatch(text):
        raise CoreError("%s must match %s" % (label, SAFE_ID_RE.pattern))
    return text


def read_bytes(path: Path, limit: int = MAX_STATE_BYTES) -> bytes:
    try:
        info = path.lstat()
    except OSError as exc:
        raise CoreError("cannot stat %s: %s" % (path, exc))
    if path.is_symlink() or not path.is_file():
        raise CoreError("state input must be a regular non-symlink file: %s" % path)
    if info.st_size > limit:
        raise CoreError("refusing oversized state file %s (%d > %d bytes)" % (path, info.st_size, limit))
    try:
        return path.read_bytes()
    except OSError as exc:
        raise CoreError("cannot read %s: %s" % (path, exc))


def read_text(path: Path, limit: int = MAX_STATE_BYTES) -> str:
    try:
        return read_bytes(path, limit).decode("utf-8")
    except UnicodeDecodeError as exc:
        raise CoreError("state file is not UTF-8: %s: %s" % (path, exc))


def read_json(path: Path, default: Any = None, limit: int = MAX_STATE_BYTES) -> Any:
    if not path.exists() and not path.is_symlink():
        return default
    try:
        return json.loads(read_text(path, limit))
    except ValueError as exc:
        raise CoreError("invalid JSON in %s: %s" % (path, exc))


def read_jsonl(path: Path, *, limit_rows: int = MAX_JSONL_ROWS) -> List[Dict[str, Any]]:
    if not path.exists() and not path.is_symlink():
        return []
    raw = read_bytes(path, MAX_JSONL_BYTES)
    rows: List[Dict[str, Any]] = []
    for number, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        if len(line) > MAX_JSONL_LINE_BYTES:
            raise CoreError("JSONL row in %s at line %d exceeds 1 MiB" % (path, number))
        if len(rows) >= limit_rows:
            raise CoreError("append-only state exceeds the supported row limit: %s" % path)
        try:
            row = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as exc:
            raise CoreError("invalid JSONL in %s at line %d: %s" % (path, number, exc))
        if not isinstance(row, dict):
            raise CoreError("JSONL row in %s at line %d is not an object" % (path, number))
        rows.append(row)
    return rows


def _ensure_state_directory_chain(path: Path) -> None:
    absolute = Path(os.path.abspath(str(path)))
    parts = absolute.parts
    indices = [index for index, part in enumerate(parts) if part == ".oms"]
    if not indices:
        absolute.mkdir(parents=True, exist_ok=True)
        return
    index = indices[-1]
    current = Path(*parts[: index + 1])
    candidates = [current]
    for end in range(index + 2, len(parts) + 1):
        candidates.append(current.joinpath(*parts[index + 1 : end]))
    for candidate in candidates:
        if candidate.exists() or candidate.is_symlink():
            if candidate.is_symlink() or not candidate.is_dir():
                raise CoreError("repository state directory must be a real directory: %s" % candidate)
        else:
            candidate.mkdir(mode=0o700)


def ensure_private_dir(path: Path) -> Path:
    _ensure_state_directory_chain(path)
    path.mkdir(parents=True, exist_ok=True)
    if path.is_symlink() or not path.is_dir():
        raise CoreError("state directory must be a real directory: %s" % path)
    with contextlib.suppress(OSError):
        path.chmod(0o700)
    return path


def atomic_write_bytes(path: Path, data: bytes, *, mode: int = 0o600) -> Path:
    parent = path.parent
    _ensure_state_directory_chain(parent)
    parent.mkdir(parents=True, exist_ok=True)
    if parent.is_symlink() or not parent.is_dir():
        raise CoreError("output directory must be a real directory: %s" % parent)
    resolved = parent.resolve(strict=True)
    target = resolved / path.name
    if target.is_symlink() or (target.exists() and not target.is_file()):
        raise CoreError("output must be a regular non-symlink file: %s" % target)
    fd, name = tempfile.mkstemp(prefix=".%s." % path.name, dir=str(resolved))
    temp = Path(name)
    try:
        if hasattr(os, "fchmod"):
            os.fchmod(fd, mode)
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(str(temp), str(target))
    except Exception:
        with contextlib.suppress(OSError):
            temp.unlink()
        raise
    return target


def atomic_write_json(path: Path, value: Any, *, mode: int = 0o600) -> Path:
    return atomic_write_bytes(path, pretty_json(value), mode=mode)


@contextlib.contextmanager
def file_lock(target: Path, timeout_seconds: float = 15.0) -> Iterator[None]:
    root = ensure_private_dir(Path(os.environ.get("OMS_LOCK_DIR", Path.home() / ".cache" / "oh-my-setting" / "locks")).expanduser())
    lock = root / ("runtime-%s.lock" % sha256_text(str(target.resolve(strict=False)))[:32])
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            lock.mkdir(mode=0o700)
            break
        except FileExistsError:
            try:
                stale = time.time() - lock.stat().st_mtime > max(30.0, timeout_seconds * 2)
            except OSError:
                stale = False
            if stale:
                with contextlib.suppress(OSError):
                    shutil.rmtree(str(lock))
                continue
            if time.monotonic() >= deadline:
                raise CoreError("timed out waiting for state lock: %s" % target, exit_code=75)
            time.sleep(0.05)
    try:
        yield
    finally:
        with contextlib.suppress(OSError):
            lock.rmdir()


def append_jsonl(path: Path, row: Mapping[str, Any]) -> None:
    encoded = canonical_json(dict(row)) + b"\n"
    if len(encoded) > MAX_JSONL_LINE_BYTES:
        raise CoreError("refusing JSONL row larger than 1 MiB")
    ensure_private_dir(path.parent)
    with file_lock(path):
        existing = b""
        if path.exists() or path.is_symlink():
            existing = read_bytes(path, MAX_JSONL_BYTES)
            if existing and not existing.endswith(b"\n"):
                raise CoreError("append-only JSONL has a partial final row: %s" % path)
        atomic_write_bytes(path, existing + encoded)


def run_output(command: Sequence[str], *, cwd: Optional[Path] = None, timeout: int = 20) -> str:
    try:
        result = subprocess.run(list(command), cwd=str(cwd) if cwd else None, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=timeout, check=False)
    except (OSError, subprocess.SubprocessError):
        return ""
    return result.stdout.strip().rstrip("\r") if result.returncode == 0 else ""


def run_json(command: Sequence[str], *, cwd: Optional[Path] = None, timeout: int = 20) -> Optional[Dict[str, Any]]:
    text = run_output(command, cwd=cwd, timeout=timeout)
    if not text:
        return None
    try:
        value = json.loads(text)
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def install_root() -> Path:
    root = Path(__file__).resolve().parents[3]
    if not (root / "scripts" / "oms").is_file():
        raise CoreError("cannot locate the OMS installation root from runtime package")
    return root


def repo_root(raw: str) -> Path:
    candidate = Path(raw).expanduser()
    root = run_output(["git", "-C", str(candidate), "rev-parse", "--show-toplevel"])
    if root:
        return Path(root).resolve()
    if not candidate.exists() or not candidate.is_dir():
        raise CoreError("repository path is not a directory: %s" % raw)
    return candidate.resolve()


def git_head(repo: Path) -> str:
    return run_output(["git", "-C", str(repo), "rev-parse", "HEAD"])


def git_branch(repo: Path) -> str:
    return run_output(["git", "-C", str(repo), "symbolic-ref", "--quiet", "--short", "HEAD"])


def relative_path(path: Path, repo: Path) -> str:
    try:
        return path.resolve().relative_to(repo.resolve()).as_posix()
    except (OSError, ValueError):
        return ""


def source_descriptor(path: Path, repo: Path) -> Optional[Dict[str, Any]]:
    if not path.is_file() or path.is_symlink():
        return None
    rel = relative_path(path, repo)
    return {"path": rel, "bytes": path.stat().st_size, "sha256": sha256_file(path)} if rel else None


def sensitive_text(text: str) -> bool:
    return bool(SECRET_VALUE_RE.search(text) or ABSOLUTE_PATH_RE.search(text))


def _redact(value: Any, limit: int = 1000) -> str:
    text = bounded_line(value, limit)
    text = ABSOLUTE_PATH_RE.sub("[absolute-path-omitted]", text)
    return "[sensitive-content-omitted]" if SECRET_VALUE_RE.search(text) else text


def sanitize_portable(value: Any, *, key: str = "") -> Any:
    blocked = {"approval", "approval_id", "lease", "lease_id", "command", "raw_command", "prompt", "transcript", "log", "log_file", "artifact", "patch", "repo", "cwd", "environment", "env", "token", "secret", "credentials", "source_path"}
    lower = key.lower()
    if lower in blocked or SECRET_NAME_RE.search(lower):
        return None
    if isinstance(value, dict):
        result: Dict[str, Any] = {}
        for raw_key, item in value.items():
            cleaned = sanitize_portable(item, key=str(raw_key))
            if cleaned is not None:
                result[str(raw_key)] = cleaned
        return result
    if isinstance(value, list):
        return [item for item in (sanitize_portable(item, key=key) for item in value) if item is not None]
    if isinstance(value, str):
        return _redact(value)
    if isinstance(value, (int, float, bool)) or value is None:
        return value
    return _redact(value)


def parse_path_list(value: Any) -> List[str]:
    items = [str(item) for item in value] if isinstance(value, (list, tuple)) else re.split(r"[,\n]", str(value or ""))
    result: List[str] = []
    for item in items:
        cleaned = item.strip().strip("`\"'").replace("\\", "/")
        while cleaned.startswith("./"):
            cleaned = cleaned[2:]
        if not cleaned or os.path.isabs(cleaned) or re.match(r"^[A-Za-z]:[\\/]", cleaned) or ".." in cleaned.split("/"):
            continue
        result.append(cleaned)
    return sorted(set(result))


def load_json_argument(value: str) -> Any:
    path = Path(value)
    if path.is_file() and not path.is_symlink():
        return read_json(path)
    try:
        return json.loads(value)
    except ValueError as exc:
        raise CoreError("expected JSON or a JSON file: %s" % exc)
