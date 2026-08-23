#!/usr/bin/env python3
"""Durable lifecycle events and approval state for the OMS control plane.

The repository-local lifecycle stream is append-only and contains no commands,
prompts, output, secrets, or absolute paths. Security-relevant approvals live
outside the repository in a per-user XDG state directory so a worktree worker
cannot authorize itself by appending to shared ``.oms`` state.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import hashlib
import hmac
import json
import math
import os
import re
import runpy
import secrets
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Dict, Iterable, Iterator, List, Optional, Sequence, Tuple


process_pid_alive = runpy.run_path(
    str(Path(__file__).with_name("process_liveness.py"))
)["pid_alive"]


SCHEMA = 1
SAFE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$")
REASON_RE = re.compile(r"^[a-z0-9][a-z0-9._:-]{0,63}$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
TERMINAL_STATES = {"done", "failed", "cancelled", "timed_out", "abandoned"}
NONTERMINAL_STATES = {
    "queued",
    "starting",
    "working",
    "waiting_input",
    "waiting_approval",
    "verifying",
    "review",
    "blocked",
}
ALL_STATES = TERMINAL_STATES | NONTERMINAL_STATES
RECONCILE_HEARTBEAT_STATES = {"starting", "working", "verifying"}
TRANSITIONS = {
    "queued": {"starting", "blocked", "failed", "timed_out", "cancelled"},
    "starting": {"working", "blocked", "failed", "timed_out", "cancelled"},
    "working": {
        "waiting_input",
        "waiting_approval",
        "verifying",
        "review",
        "blocked",
        "failed",
        "timed_out",
        "cancelled",
    },
    "waiting_input": {"working", "blocked", "failed", "timed_out", "cancelled"},
    "waiting_approval": {"working", "blocked", "failed", "timed_out", "cancelled"},
    "verifying": {"review", "working", "blocked", "failed", "timed_out", "cancelled"},
    "review": {"done", "working", "blocked", "failed", "cancelled"},
    "blocked": {"working", "failed", "timed_out", "cancelled", "abandoned"},
}
APPROVAL_STATES = {
    "requested",
    "approved",
    "denied",
    "expired",
    "consuming",
    "consumed",
    "failed",
    "interrupted",
}


class OpsError(Exception):
    """A user-facing contract error."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_ts(value: str) -> dt.datetime:
    try:
        return dt.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
    except (TypeError, ValueError) as exc:
        raise OpsError("invalid UTC timestamp: %s" % value) from exc


def json_bytes(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def safe_id(value: str, label: str, *, optional: bool = False) -> str:
    value = value or ""
    if not value and optional:
        return ""
    if not SAFE_ID_RE.fullmatch(value):
        raise OpsError("%s must match [A-Za-z0-9][A-Za-z0-9._:-]{0,159}" % label)
    return value


def safe_reason(value: str) -> str:
    if not value:
        return ""
    if not REASON_RE.fullmatch(value):
        raise OpsError("reason_code must match [a-z0-9][a-z0-9._:-]{0,63}")
    return value


def bounded_text(value: str, label: str, limit: int = 200) -> str:
    if not value or len(value) > limit or any(ch in value for ch in "\r\n\t"):
        raise OpsError("%s must be one line of 1-%d characters" % (label, limit))
    # Approval descriptions are durable state. Reject common credential shapes
    # and absolute machine paths rather than pretending a private file makes a
    # leaked secret harmless.
    sensitive = re.compile(
        r"(?i)(api[_-]?key|access[_-]?token|secret|password|private[_-]?key)\s*[:=]|"
        r"-----BE" r"GIN [A-Z ]+PRIVATE " r"KEY-----|(^|\s)(/Us" r"ers/|/ho" r"me/|[A-Za-z]:[\\/])"
    )
    if sensitive.search(value):
        raise OpsError("%s contains sensitive-looking content or an absolute machine path" % label)
    return value


def repo_root(raw: str) -> Path:
    path = Path(raw).expanduser()
    try:
        out = subprocess.check_output(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8", "replace").strip().rstrip("\r")
        if out:
            return Path(out).resolve()
    except (OSError, subprocess.CalledProcessError):
        pass
    if not path.exists():
        raise OpsError("repository path does not exist: %s" % raw)
    return path.resolve()


def git_head(repo: Path) -> str:
    try:
        value = subprocess.check_output(
            ["git", "-C", str(repo), "rev-parse", "HEAD"], stderr=subprocess.DEVNULL
        ).decode("ascii", "replace").strip().rstrip("\r")
        return value if re.fullmatch(r"[0-9a-f]{40,64}", value) else ""
    except (OSError, subprocess.CalledProcessError):
        return ""


def ensure_oms(repo: Path) -> Path:
    oms = repo / ".oms"
    oms.mkdir(parents=True, exist_ok=True)
    ignore = oms / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n", encoding="utf-8")
    lifecycle = oms / "lifecycle"
    lifecycle.mkdir(parents=True, exist_ok=True)
    return lifecycle


def event_path(repo: Path) -> Path:
    return repo / ".oms" / "lifecycle" / "events.jsonl"


def approval_path(repo: Path, *, create: bool = False) -> Path:
    state_home = os.environ.get("XDG_STATE_HOME")
    if state_home:
        base = Path(state_home).expanduser()
    elif os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        base = Path(os.environ["LOCALAPPDATA"])
    else:
        base = Path.home() / ".local" / "state"
    directory = base / "oh-my-setting" / "approvals"
    if create:
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        try:
            directory.chmod(0o700)
        except OSError:
            pass
    digest = hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest()
    return directory / (digest + ".jsonl")


def runtime_lock_dir(path: Path) -> Path:
    root = os.environ.get("OMS_LOCK_DIR")
    if root:
        base = Path(root).expanduser()
    else:
        base = Path.home() / ".cache" / "oh-my-setting" / "locks"
    base.mkdir(parents=True, exist_ok=True)
    key = hashlib.sha256(str(path.resolve()).encode("utf-8")).hexdigest()[:32]
    return base / ("agent-events-" + key + ".lockdir")


def pid_alive(pid: int) -> bool:
    # agent-events lock owners are native Python processes, so their stored
    # pid is already in the Win32 domain on Windows.
    return process_pid_alive(pid, native_pid=pid)


def process_start_token(pid: int) -> Optional[str]:
    """Return a stable process-generation token when the host exposes one."""
    if pid <= 0:
        return None
    if os.name == "nt":
        try:
            import ctypes
            from ctypes import wintypes

            process_query_limited_information = 0x1000
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
            kernel32.OpenProcess.restype = wintypes.HANDLE
            kernel32.GetProcessTimes.argtypes = [
                wintypes.HANDLE,
                ctypes.POINTER(wintypes.FILETIME),
                ctypes.POINTER(wintypes.FILETIME),
                ctypes.POINTER(wintypes.FILETIME),
                ctypes.POINTER(wintypes.FILETIME),
            ]
            kernel32.GetProcessTimes.restype = wintypes.BOOL
            kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
            kernel32.CloseHandle.restype = wintypes.BOOL
            handle = kernel32.OpenProcess(
                process_query_limited_information, False, pid
            )
            if not handle:
                return None
            creation = wintypes.FILETIME()
            exit_time = wintypes.FILETIME()
            kernel = wintypes.FILETIME()
            user = wintypes.FILETIME()
            try:
                ok = kernel32.GetProcessTimes(
                    handle,
                    ctypes.byref(creation),
                    ctypes.byref(exit_time),
                    ctypes.byref(kernel),
                    ctypes.byref(user),
                )
                if not ok:
                    return None
                value = (creation.dwHighDateTime << 32) | creation.dwLowDateTime
                return "win:%d" % value
            finally:
                kernel32.CloseHandle(handle)
        except (AttributeError, OSError, ValueError):
            return None

    proc_stat = Path("/proc") / str(pid) / "stat"
    try:
        line = proc_stat.read_text(encoding="utf-8", errors="replace")
        rest = line.rsplit(") ", 1)[1].split()
        if len(rest) >= 20 and rest[19].isdigit():
            return "proc:%s" % rest[19]
    except (IndexError, OSError):
        pass

    try:
        env = dict(os.environ)
        env["LC_ALL"] = "C"
        value = subprocess.check_output(
            ["ps", "-p", str(pid), "-o", "lstart="],
            stderr=subprocess.DEVNULL,
            env=env,
        ).decode("utf-8", "replace")
        value = " ".join(value.replace("\r", "").split())
        return "ps:%s" % value if value else None
    except (OSError, subprocess.CalledProcessError):
        return None


def positive_env_seconds(name: str, default: int) -> int:
    try:
        return max(1, int(os.environ.get(name, str(default))))
    except ValueError:
        return default


def lock_snapshot(lock: Path) -> Optional[Dict[str, Any]]:
    """Read one stable lock generation, including malformed/partial owners."""
    owner_path = lock / "owner.json"
    for _ in range(3):
        try:
            before = lock.stat()
            try:
                raw = owner_path.read_bytes()
            except FileNotFoundError:
                raw = b""
            after = lock.stat()
        except FileNotFoundError:
            return None
        except OSError:
            return None
        before_identity = (
            before.st_dev,
            before.st_ino,
            getattr(before, "st_mtime_ns", int(before.st_mtime * 1_000_000_000)),
            getattr(before, "st_ctime_ns", int(before.st_ctime * 1_000_000_000)),
        )
        after_identity = (
            after.st_dev,
            after.st_ino,
            getattr(after, "st_mtime_ns", int(after.st_mtime * 1_000_000_000)),
            getattr(after, "st_ctime_ns", int(after.st_ctime * 1_000_000_000)),
        )
        if before_identity != after_identity:
            continue
        generation = hashlib.sha256(
            repr(after_identity).encode("ascii") + b"\0" + raw
        ).hexdigest()
        owner: Optional[Dict[str, Any]] = None
        if raw:
            try:
                candidate = json.loads(raw.decode("utf-8"))
                if isinstance(candidate, dict):
                    owner = candidate
            except (UnicodeDecodeError, json.JSONDecodeError):
                pass
        return {
            "generation": generation,
            "owner": owner,
            "mtime": after.st_mtime,
        }
    return None


def lock_snapshot_stale(snapshot: Dict[str, Any], unknown_stale_seconds: int) -> bool:
    owner = snapshot.get("owner")
    if isinstance(owner, dict):
        try:
            pid = int(owner.get("pid", 0))
        except (TypeError, ValueError):
            pid = 0
        owner_nonce = owner.get("owner_nonce")
        if pid > 0 and isinstance(owner_nonce, str) and owner_nonce:
            if not pid_alive(pid):
                return True
            recorded_start = owner.get("process_start")
            if isinstance(recorded_start, str) and recorded_start:
                actual_start = process_start_token(pid)
                if actual_start and actual_start != recorded_start:
                    return True
            # Timeout is a wait bound, never a lease on a living generation.
            return False
    try:
        return time.time() - float(snapshot["mtime"]) > unknown_stale_seconds
    except (KeyError, TypeError, ValueError):
        return False


@contextlib.contextmanager
def recovery_gate(lock: Path) -> Iterator[bool]:
    """Serialize stale rechecks; the kernel releases this lock on a crash."""
    gate = lock.with_name(lock.name + ".recovery")
    flags = os.O_RDWR | os.O_CREAT
    try:
        fd = os.open(str(gate), flags, 0o600)
    except OSError:
        yield False
        return
    acquired = False
    try:
        try:
            os.chmod(gate, 0o600)
        except OSError:
            pass
        if os.name == "nt":
            import msvcrt

            if os.fstat(fd).st_size < 1:
                os.write(fd, b"\0")
            os.lseek(fd, 0, os.SEEK_SET)
            try:
                msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
                acquired = True
            except OSError:
                acquired = False
        else:
            import fcntl

            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                acquired = True
            except (BlockingIOError, OSError):
                acquired = False
        yield acquired
    finally:
        if acquired:
            try:
                if os.name == "nt":
                    import msvcrt

                    os.lseek(fd, 0, os.SEEK_SET)
                    msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
                else:
                    import fcntl

                    fcntl.flock(fd, fcntl.LOCK_UN)
            except OSError:
                pass
        os.close(fd)


def reclaim_lock(
    lock: Path,
    expected: Optional[Dict[str, Any]] = None,
    unknown_stale_seconds: int = 300,
) -> bool:
    with recovery_gate(lock) as acquired:
        if not acquired:
            return False
        current = lock_snapshot(lock)
        if current is None:
            return False
        if expected is not None and current["generation"] != expected.get("generation"):
            return False
        if not lock_snapshot_stale(current, unknown_stale_seconds):
            return False
        stale_name = lock.with_name(lock.name + ".stale." + secrets.token_hex(8))
        try:
            lock.rename(stale_name)
        except OSError:
            return False
        shutil.rmtree(stale_name, ignore_errors=True)
        return True


@contextlib.contextmanager
def file_lock(path: Path) -> Iterator[None]:
    lock = runtime_lock_dir(path)
    wait_seconds = positive_env_seconds("OMS_LOCK_TIMEOUT", 300)
    unknown_owner_stale_seconds = positive_env_seconds("OMS_LOCK_STALE_SECONDS", 300)
    deadline = time.monotonic() + wait_seconds
    owner_nonce = "%d.%s" % (os.getpid(), secrets.token_hex(8))
    current_process_start = process_start_token(os.getpid()) or ""
    while True:
        try:
            lock.mkdir()
            try:
                (lock / "owner.json").write_text(
                    json.dumps(
                        {
                            "pid": os.getpid(),
                            "process_start": current_process_start,
                            "owner_nonce": owner_nonce,
                            "started": time.time(),
                        }
                    ),
                    encoding="utf-8",
                )
            except OSError:
                shutil.rmtree(lock, ignore_errors=True)
                raise
            break
        except FileExistsError:
            snapshot = lock_snapshot(lock)
            if snapshot is None:
                if time.monotonic() >= deadline:
                    raise OpsError("could not acquire state lock for %s" % path)
                continue
            if lock_snapshot_stale(snapshot, unknown_owner_stale_seconds) and reclaim_lock(
                lock, snapshot, unknown_owner_stale_seconds
            ):
                continue
            if time.monotonic() >= deadline:
                raise OpsError("could not acquire state lock for %s" % path)
            time.sleep(min(0.05, max(0.001, deadline - time.monotonic())))
    try:
        yield
    finally:
        try:
            owner = json.loads((lock / "owner.json").read_text(encoding="utf-8"))
            if owner.get("owner_nonce") == owner_nonce:
                (lock / "owner.json").unlink()
                lock.rmdir()
        except (OSError, json.JSONDecodeError):
            pass


def read_rows(path: Path) -> List[Dict[str, Any]]:
    if not path.exists():
        return []
    rows: List[Dict[str, Any]] = []
    with path.open(encoding="utf-8", errors="replace") as handle:
        for number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as exc:
                raise OpsError("%s:%d is not valid JSON: %s" % (path, number, exc)) from exc
            if not isinstance(row, dict):
                raise OpsError("%s:%d is not a JSON object" % (path, number))
            rows.append(row)
    return rows


def write_all(fd: int, data: bytes) -> None:
    remaining = memoryview(data)
    while remaining:
        try:
            written = os.write(fd, remaining)
        except InterruptedError:
            continue
        if written <= 0:
            raise OSError("write returned no progress")
        remaining = remaining[written:]


def fsync_directory(path: Path) -> None:
    # Windows does not support opening directories through os.open. POSIX
    # platforms used by the supported lifecycle do, including macOS Bash 3.2.
    if os.name == "nt":
        return
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    fd = os.open(str(path), flags)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def append_row(path: Path, row: Dict[str, Any], *, private: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND
    fd = os.open(str(path), flags, 0o600 if private else 0o644)
    try:
        write_all(fd, json_bytes(row) + b"\n")
        os.fsync(fd)
    finally:
        os.close(fd)
    if private:
        try:
            path.chmod(0o600)
        except OSError:
            pass
    fsync_directory(path.parent)


def write_json_atomic(path: Path, value: Dict[str, Any], mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(prefix=path.name + ".", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(value, handle, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp_name, mode)
        os.replace(temp_name, path)
        fsync_directory(path.parent)
    except Exception:
        try:
            os.unlink(temp_name)
        except OSError:
            pass
        raise


def effective_run_id(repo: Path) -> str:
    explicit = os.environ.get("OMS_RUN_ID", "")
    if explicit:
        return safe_id(explicit, "OMS_RUN_ID")
    current = repo / ".oms" / "runs" / "CURRENT"
    try:
        parts = current.read_text(encoding="utf-8").split()
        ttl = int(os.environ.get("OMS_RUN_CURRENT_TTL", "86400"))
        if len(parts) >= 2 and parts[1].isdigit() and time.time() - int(parts[1]) <= ttl:
            return safe_id(parts[0].rstrip("\r"), "current run id")
    except (OSError, ValueError, OpsError):
        pass
    return "run_" + uuid.uuid4().hex


def lifecycle_semantics(row: Dict[str, Any]) -> Dict[str, Any]:
    return {
        key: value
        for key, value in row.items()
        # from_state and sequence are allocated under the append lock. Callers
        # repeat the desired effect, not those derived fields, on an
        # idempotent retry.
        if key not in {"event_id", "ts", "seq", "from_state"}
    }


def validate_event_row(row: Dict[str, Any], number: int = 0) -> None:
    where = "event row %d" % number if number else "event row"
    if row.get("schema") != SCHEMA:
        raise OpsError("%s has unsupported schema" % where)
    safe_id(str(row.get("event_id", "")), "event_id")
    safe_id(str(row.get("attempt_id", "")), "attempt_id")
    parse_ts(str(row.get("ts", "")))
    seq = row.get("seq")
    if not isinstance(seq, int) or isinstance(seq, bool) or seq < 1:
        raise OpsError("%s has invalid seq" % where)
    event_type = row.get("event_type")
    if event_type not in {"attempt.created", "attempt.state_changed", "attempt.heartbeat", "attempt.usage"}:
        raise OpsError("%s has invalid event_type" % where)
    for key in ("run_id", "task_id", "parent_attempt_id", "provider", "tool", "idempotency_key"):
        value = row.get(key, "")
        if value:
            safe_id(str(value), key)
    reason = row.get("reason_code", "")
    if reason:
        safe_reason(str(reason))
    state = row.get("to_state")
    if state is not None and state not in ALL_STATES:
        raise OpsError("%s has invalid to_state" % where)
    old = row.get("from_state")
    if old is not None and old not in ALL_STATES:
        raise OpsError("%s has invalid from_state" % where)
    usage = row.get("usage", {})
    if usage:
        if not isinstance(usage, dict):
            raise OpsError("%s usage is not an object" % where)
        for key in ("tokens", "duration_ms", "cost_microusd"):
            value = usage.get(key, 0)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise OpsError("%s has invalid usage.%s" % (where, key))
    refs = row.get("refs", {})
    if refs:
        if not isinstance(refs, dict):
            raise OpsError("%s refs is not an object" % where)
        for key, value in refs.items():
            safe_id(str(key), "ref name")
            if not isinstance(value, str) or len(value) > 240 or "\n" in value or "\r" in value:
                raise OpsError("%s has invalid ref %s" % (where, key))
            if value.startswith(("/", "\\")) or re.match(r"^[A-Za-z]:[\\/]", value) or ".." in Path(value).parts:
                raise OpsError("%s contains an absolute or escaping ref" % where)


def project_attempts(rows: Sequence[Dict[str, Any]], *, validate: bool = True) -> Dict[str, Dict[str, Any]]:
    attempts: Dict[str, Dict[str, Any]] = {}
    event_ids = set()
    idempotency: Dict[Tuple[str, str], Dict[str, Any]] = {}
    for number, row in enumerate(rows, 1):
        if validate:
            validate_event_row(row, number)
        event_id = row["event_id"]
        if event_id in event_ids:
            raise OpsError("duplicate lifecycle event_id: %s" % event_id)
        event_ids.add(event_id)
        attempt_id = row["attempt_id"]
        current = attempts.get(attempt_id)
        expected_seq = 1 if current is None else int(current["sequence"]) + 1
        if row["seq"] != expected_seq:
            raise OpsError(
                "attempt %s sequence is %s, expected %s" % (attempt_id, row["seq"], expected_seq)
            )
        key = row.get("idempotency_key")
        if key:
            marker = (attempt_id, key)
            if marker in idempotency:
                raise OpsError("duplicate idempotency key was appended: %s" % key)
            idempotency[marker] = row
        if row["event_type"] == "attempt.created":
            if current is not None:
                raise OpsError("attempt %s was created twice" % attempt_id)
            if row.get("from_state") is not None or row.get("to_state") != "queued":
                raise OpsError("attempt.created must transition null -> queued")
            current = {
                "schema": SCHEMA,
                "attempt_id": attempt_id,
                "run_id": row.get("run_id", ""),
                "task_id": row.get("task_id", ""),
                "parent_attempt_id": row.get("parent_attempt_id", ""),
                "provider": row.get("provider", ""),
                "tool": row.get("tool", ""),
                "base_sha": row.get("base_sha", ""),
                "state": "queued",
                "sequence": row["seq"],
                "created_at": row["ts"],
                "updated_at": row["ts"],
                "last_heartbeat_at": row["ts"],
                "reason_code": row.get("reason_code", ""),
                "usage": {"tokens": 0, "duration_ms": 0, "cost_microusd": 0},
                "usage_reports": {"tokens": 0, "duration_ms": 0, "cost_microusd": 0},
                "budget": dict(row.get("budget", {})),
                "refs": dict(row.get("refs", {})),
                "terminal": False,
            }
            attempts[attempt_id] = current
            continue
        if current is None:
            raise OpsError("event precedes attempt.created for %s" % attempt_id)
        event_type = row["event_type"]
        if event_type == "attempt.state_changed":
            old = current["state"]
            new = row.get("to_state")
            if row.get("from_state") != old:
                raise OpsError("attempt %s from_state does not match projection" % attempt_id)
            if old in TERMINAL_STATES or new not in TRANSITIONS.get(old, set()):
                raise OpsError("illegal lifecycle transition: %s -> %s" % (old, new))
            current["state"] = new
            current["terminal"] = new in TERMINAL_STATES
            current["reason_code"] = row.get("reason_code", "")
        elif event_type == "attempt.usage":
            for name, value in row.get("usage", {}).items():
                current["usage"][name] += int(value)
                current["usage_reports"][name] += 1
        current["sequence"] = row["seq"]
        current["updated_at"] = row["ts"]
        if event_type in {"attempt.heartbeat", "attempt.state_changed"}:
            current["last_heartbeat_at"] = row["ts"]
        if row.get("refs"):
            current["refs"].update(row["refs"])
    return attempts


def next_seq(rows: Sequence[Dict[str, Any]], attempt_id: str) -> int:
    return 1 + sum(1 for row in rows if row.get("attempt_id") == attempt_id)


def existing_idempotent(rows: Sequence[Dict[str, Any]], candidate: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    key = candidate.get("idempotency_key")
    if not key:
        return None
    for row in rows:
        if row.get("attempt_id") == candidate.get("attempt_id") and row.get("idempotency_key") == key:
            if lifecycle_semantics(row) == lifecycle_semantics(candidate):
                return row
            raise OpsError("idempotency key already names a different lifecycle event: %s" % key)
    return None


def new_event(attempt_id: str, seq: int, event_type: str, **fields: Any) -> Dict[str, Any]:
    row: Dict[str, Any] = {
        "schema": SCHEMA,
        "event_id": "aevt_" + uuid.uuid4().hex,
        "ts": utc_now(),
        "attempt_id": attempt_id,
        "seq": seq,
        "event_type": event_type,
    }
    for key, value in fields.items():
        if value not in (None, "", {}, []):
            row[key] = value
        elif key in {"from_state"}:
            row[key] = value
    return row


def append_lifecycle(repo: Path, candidate: Dict[str, Any]) -> Dict[str, Any]:
    ensure_oms(repo)
    path = event_path(repo)
    with file_lock(path):
        rows = read_rows(path)
        projection = project_attempts(rows)
        prior = existing_idempotent(rows, candidate)
        if prior is not None:
            return prior
        attempt_id = candidate["attempt_id"]
        event_type = candidate["event_type"]
        if event_type == "attempt.created":
            if attempt_id in projection:
                raise OpsError("attempt already exists: %s" % attempt_id)
            candidate["seq"] = 1
        else:
            if attempt_id not in projection:
                raise OpsError("unknown attempt: %s" % attempt_id)
            candidate["seq"] = next_seq(rows, attempt_id)
            if event_type == "attempt.state_changed":
                old = projection[attempt_id]["state"]
                new = candidate.get("to_state")
                candidate["from_state"] = old
                if old in TERMINAL_STATES or new not in TRANSITIONS.get(old, set()):
                    raise OpsError("illegal lifecycle transition: %s -> %s" % (old, new))
        validate_event_row(candidate)
        append_row(path, candidate)
        return candidate


def attempt_spec_path(repo: Path, attempt_id: str) -> Path:
    return repo / ".oms" / "lifecycle" / "attempts" / attempt_id / "spec.json"


def attempt_spec_from_created(row: Dict[str, Any]) -> Dict[str, Any]:
    return {
        "schema": SCHEMA,
        "attempt_id": row["attempt_id"],
        "run_id": row.get("run_id", ""),
        "task_id": row.get("task_id", ""),
        "parent_attempt_id": row.get("parent_attempt_id", ""),
        "provider": row.get("provider", ""),
        "tool": row.get("tool", ""),
        "base_sha": row.get("base_sha", ""),
        "budget": dict(row.get("budget", {})),
        "refs": dict(row.get("refs", {})),
        "created_at": row["ts"],
    }


def start_request_semantics(row: Dict[str, Any], *, include_run_id: bool) -> Dict[str, Any]:
    value = {
        "provider": row.get("provider", ""),
        "tool": row.get("tool", ""),
        "task_id": row.get("task_id", ""),
        "parent_attempt_id": row.get("parent_attempt_id", ""),
        "budget": dict(row.get("budget", {})),
        "refs": dict(row.get("refs", {})),
    }
    if include_run_id:
        value["run_id"] = row.get("run_id", "")
    return value


def create_attempt(
    repo: Path,
    *,
    provider: str,
    tool: str,
    task_id: str = "",
    run_id: str = "",
    parent_attempt_id: str = "",
    budget: Optional[Dict[str, int]] = None,
    refs: Optional[Dict[str, str]] = None,
    idempotency_key: str = "",
) -> str:
    ensure_oms(repo)
    provider = safe_id(provider or "local", "provider")
    tool = safe_id(tool, "tool")
    task_id = safe_id(task_id, "task_id", optional=True)
    parent_attempt_id = safe_id(parent_attempt_id, "parent_attempt_id", optional=True)
    requested_run_id = safe_id(run_id, "run_id") if run_id else ""
    idempotency_key = safe_id(idempotency_key, "idempotency_key", optional=True)
    clean_budget = {key: int(value) for key, value in (budget or {}).items() if value is not None}
    for key, value in clean_budget.items():
        if key not in {"max_wall_seconds", "max_tokens", "max_cost_microusd"} or value < 0:
            raise OpsError("invalid attempt budget: %s" % key)
    clean_refs = dict(refs or {})
    requested = {
        "provider": provider,
        "tool": tool,
        "task_id": task_id,
        "parent_attempt_id": parent_attempt_id,
        "budget": clean_budget,
        "refs": clean_refs,
    }
    if requested_run_id:
        requested["run_id"] = requested_run_id

    path = event_path(repo)
    with file_lock(path):
        rows = read_rows(path)
        projection = project_attempts(rows)
        if idempotency_key:
            matches = [
                row for row in rows
                if row.get("event_type") == "attempt.created"
                and row.get("idempotency_key") == idempotency_key
            ]
            if len(matches) > 1:
                raise OpsError("start idempotency key names multiple attempts: %s" % idempotency_key)
            if matches:
                prior = matches[0]
                if start_request_semantics(prior, include_run_id=bool(requested_run_id)) != requested:
                    raise OpsError("idempotency key already names a different start request: %s" % idempotency_key)
                prior_spec = attempt_spec_from_created(prior)
                spec_path = attempt_spec_path(repo, prior["attempt_id"])
                if spec_path.exists():
                    try:
                        stored_spec = json.loads(spec_path.read_text(encoding="utf-8"))
                    except (OSError, json.JSONDecodeError) as exc:
                        raise OpsError("attempt has no valid durable spec: %s" % prior["attempt_id"]) from exc
                    if not isinstance(stored_spec, dict) or any(
                        stored_spec.get(key) != value for key, value in prior_spec.items()
                    ):
                        raise OpsError("attempt spec does not match its creation event: %s" % prior["attempt_id"])
                else:
                    write_json_atomic(spec_path, prior_spec)
                return str(prior["attempt_id"])

        run_id = requested_run_id or effective_run_id(repo)
        attempt_id = "att_" + uuid.uuid4().hex
        event = new_event(
            attempt_id,
            1,
            "attempt.created",
            run_id=run_id,
            task_id=task_id,
            parent_attempt_id=parent_attempt_id,
            provider=provider,
            tool=tool,
            base_sha=git_head(repo),
            from_state=None,
            to_state="queued",
            budget=clean_budget,
            refs=clean_refs,
            actor={"kind": "owner", "name": "agent-events"},
            idempotency_key=idempotency_key,
        )
        if attempt_id in projection:
            raise OpsError("attempt already exists: %s" % attempt_id)
        validate_event_row(event)
        append_row(path, event)
        write_json_atomic(attempt_spec_path(repo, attempt_id), attempt_spec_from_created(event))
        return attempt_id


def event_start(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    budget = {
        "max_wall_seconds": args.max_wall_seconds,
        "max_tokens": args.max_tokens,
        "max_cost_microusd": args.max_cost_microusd,
    }
    attempt = create_attempt(
        repo,
        provider=args.provider,
        tool=args.tool,
        task_id=args.task_id,
        run_id=args.run_id,
        parent_attempt_id=args.parent_attempt_id,
        budget=budget,
        idempotency_key=args.idempotency_key,
    )
    print(attempt)
    return 0


def event_transition(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    attempt = safe_id(args.attempt, "attempt")
    state = args.state
    if state not in ALL_STATES:
        raise OpsError("invalid lifecycle state: %s" % state)
    event = new_event(
        attempt,
        0,
        "attempt.state_changed",
        from_state=None,
        to_state=state,
        reason_code=safe_reason(args.reason_code),
        actor={"kind": safe_id(args.actor_kind, "actor kind"), "name": safe_id(args.actor, "actor")},
        idempotency_key=safe_id(args.idempotency_key, "idempotency_key", optional=True),
    )
    row = append_lifecycle(repo, event)
    print(row["event_id"])
    return 0


def event_heartbeat(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    attempt = safe_id(args.attempt, "attempt")
    event = new_event(
        attempt,
        0,
        "attempt.heartbeat",
        actor={"kind": "runner", "name": safe_id(args.actor, "actor")},
        idempotency_key=safe_id(args.idempotency_key, "idempotency_key", optional=True),
    )
    row = append_lifecycle(repo, event)
    print(row["event_id"])
    return 0


def event_usage(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    attempt = safe_id(args.attempt, "attempt")
    usage = {
        key: value
        for key, value in {
            "tokens": args.tokens,
            "duration_ms": args.duration_ms,
            "cost_microusd": args.cost_microusd,
        }.items()
        if value is not None
    }
    if not usage:
        raise OpsError("usage requires at least one measured value")
    if any(value < 0 for value in usage.values()):
        raise OpsError("usage values must be non-negative integers")
    event = new_event(
        attempt,
        0,
        "attempt.usage",
        usage=usage,
        actor={"kind": "telemetry", "name": safe_id(args.actor, "actor")},
        idempotency_key=safe_id(args.idempotency_key, "idempotency_key", optional=True),
    )
    row = append_lifecycle(repo, event)
    print(row["event_id"])
    return 0


def load_projection(repo: Path) -> Tuple[List[Dict[str, Any]], Dict[str, Dict[str, Any]]]:
    rows = read_rows(event_path(repo))
    return rows, project_attempts(rows)


def print_json(value: Any) -> None:
    print(json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2))


def event_show(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    _, attempts = load_projection(repo)
    attempt = attempts.get(safe_id(args.attempt, "attempt"))
    if attempt is None:
        raise OpsError("unknown attempt: %s" % args.attempt)
    if args.json:
        print_json(attempt)
    else:
        print("attempt: %s" % attempt["attempt_id"])
        print("state: %s" % attempt["state"])
        print("provider/tool: %s/%s" % (attempt.get("provider") or "-", attempt.get("tool") or "-"))
        print("run/task: %s/%s" % (attempt.get("run_id") or "-", attempt.get("task_id") or "-"))
        print("usage: tokens=%d duration_ms=%d cost_microusd=%d" % (
            attempt["usage"]["tokens"], attempt["usage"]["duration_ms"], attempt["usage"]["cost_microusd"]
        ))
    return 0


def event_list(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    _, attempts = load_projection(repo)
    values = list(attempts.values())
    values.sort(key=lambda item: item["updated_at"])
    if args.active:
        values = [item for item in values if not item["terminal"]]
    if args.state:
        values = [item for item in values if item["state"] == args.state]
    if args.limit:
        values = values[-args.limit :]
    if args.json:
        print_json(values)
    elif not values:
        print("no attempts")
    else:
        for item in values:
            print("%-38s %-18s %-12s %-16s %s" % (
                item["attempt_id"], item["state"], item.get("provider") or "-", item.get("tool") or "-", item["updated_at"]
            ))
    return 0


def event_resume(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    _, attempts = load_projection(repo)
    parent_id = safe_id(args.attempt, "attempt")
    parent = attempts.get(parent_id)
    if parent is None:
        raise OpsError("unknown attempt: %s" % parent_id)
    if parent["state"] not in TERMINAL_STATES | {"blocked"}:
        raise OpsError("attempt is still live; attach or wait instead of duplicating it")
    spec_file = attempt_spec_path(repo, parent_id)
    try:
        spec = json.loads(spec_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise OpsError("attempt has no valid durable spec: %s" % parent_id) from exc
    if spec.get("base_sha") and git_head(repo) != spec["base_sha"]:
        raise OpsError("repository HEAD changed; the prior attempt cannot be resumed safely")
    attempt = create_attempt(
        repo,
        provider=spec.get("provider", "local"),
        tool=spec.get("tool", "agent-run"),
        task_id=spec.get("task_id", ""),
        run_id=spec.get("run_id", ""),
        parent_attempt_id=parent_id,
        budget=spec.get("budget", {}),
        refs=spec.get("refs", {}),
    )
    print(attempt)
    return 0


def event_follow(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = event_path(repo)
    after = args.after_event
    emitted = 0
    deadline = None if args.timeout < 0 else time.monotonic() + args.timeout
    seen_after = not after
    offset = 0
    while True:
        rows = read_rows(path)
        start = offset
        if after and offset == 0:
            start = len(rows)
            for index, row in enumerate(rows):
                if row.get("event_id") == after:
                    start = index + 1
                    seen_after = True
                    break
            if not seen_after:
                raise OpsError("after-event was not found: %s" % after)
        for row in rows[start:]:
            print(json.dumps(row, ensure_ascii=False, allow_nan=False, separators=(",", ":")), flush=True)
            emitted += 1
            if args.limit and emitted >= args.limit:
                return 0
        offset = len(rows)
        if args.timeout == 0 or (deadline is not None and time.monotonic() >= deadline):
            return 0
        time.sleep(args.interval)


def event_reconcile(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    if args.stale_seconds < 0:
        raise OpsError("stale-seconds must be non-negative")
    path = event_path(repo)
    stale: List[Dict[str, Any]] = []
    # Selection and apply share one lock snapshot. A heartbeat that wins the
    # lock is therefore visible before expiry, and no heartbeat can land
    # between the decision and its state transition.
    with file_lock(path):
        rows = read_rows(path)
        attempts = project_attempts(rows)
        now = dt.datetime.now(dt.timezone.utc)
        for item in attempts.values():
            if item["state"] not in RECONCILE_HEARTBEAT_STATES:
                continue
            age = int((now - parse_ts(item["last_heartbeat_at"])).total_seconds())
            if age >= args.stale_seconds:
                stale.append({
                    "attempt_id": item["attempt_id"],
                    "state": item["state"],
                    "age_seconds": age,
                    "sequence": item["sequence"],
                })
        if args.apply:
            for item in stale:
                event = new_event(
                    item["attempt_id"], next_seq(rows, item["attempt_id"]),
                    "attempt.state_changed", from_state=item["state"],
                    to_state="blocked", reason_code="heartbeat_expired",
                    actor={"kind": "reconciler", "name": "agent-events"},
                    idempotency_key="reconcile-stale-%d" % item["sequence"],
                )
                if existing_idempotent(rows, event) is not None:
                    raise OpsError(
                        "reconcile idempotency key already exists for this stale generation: %s"
                        % event["idempotency_key"]
                    )
                validate_event_row(event)
                append_row(path, event)
                rows.append(event)
    # Sequence is an internal stale-generation marker, not part of the
    # command's established output contract.
    for item in stale:
        item.pop("sequence", None)
    if args.json:
        print_json(stale)
    else:
        for item in stale:
            print("%s %s age=%ss%s" % (
                item["attempt_id"], item["state"], item["age_seconds"], " -> blocked" if args.apply else ""
            ))
        if not stale:
            print("no stale attempts")
    return 0


def event_validate(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    rows = read_rows(event_path(repo))
    project_attempts(rows)
    print("lifecycle: ok (%d events)" % len(rows))
    return 0


def event_compact(args: argparse.Namespace) -> int:
    """Drop the full event streams of long-terminal attempts.

    The stream is append-only and no writer prunes it, so the projection
    every reader consumes grows without bound. Compaction removes whole
    attempts only — an attempt's stream is valid from its first row or not
    at all — and only attempts that are terminal and quiet past the cutoff.
    Kept rows keep their original bytes, the survivors must still project
    before the replace, and a stream that will not project refuses to
    compact at all (fail closed).
    """
    repo = repo_root(args.repo)
    path = event_path(repo)
    if not path.exists():
        print("lifecycle-compact: no events file")
        return 0
    days = int(args.days)
    if days < 0:
        raise OpsError("--days must be non-negative")
    cutoff = (
        (dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=days))
        .replace(microsecond=0)
        .strftime("%Y-%m-%dT%H:%M:%SZ")
    )

    def load() -> Tuple[List[str], List[Dict[str, Any]]]:
        raws: List[str] = []
        rows: List[Dict[str, Any]] = []
        with path.open(encoding="utf-8", errors="replace") as handle:
            for number, line in enumerate(handle, 1):
                if not line.strip():
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError as exc:
                    raise OpsError("%s:%d is not valid JSON: %s" % (path, number, exc)) from exc
                rows.append(row)
                raws.append(line if line.endswith("\n") else line + "\n")
        return raws, rows

    def plan(raws: List[str], rows: List[Dict[str, Any]]):
        attempts = project_attempts(rows)
        drop = {
            attempt_id
            for attempt_id, item in attempts.items()
            if item.get("terminal") and str(item.get("updated_at", "")) < cutoff
        }
        kept_raws = []
        kept_rows = []
        for raw, row in zip(raws, rows):
            if row.get("attempt_id") in drop:
                continue
            kept_raws.append(raw)
            kept_rows.append(row)
        return drop, kept_raws, kept_rows

    if not args.apply:
        raws, rows = load()
        drop, kept_raws, _ = plan(raws, rows)
        print(
            "lifecycle-compact: would drop %d event(s) across %d terminal attempt(s)"
            " older than %dd (keeping %d)"
            % (len(rows) - len(kept_raws), len(drop), days, len(kept_raws))
        )
        return 0

    with file_lock(path):
        raws, rows = load()
        drop, kept_raws, kept_rows = plan(raws, rows)
        if not drop:
            print("lifecycle-compact: nothing to drop (%d events)" % len(rows))
            return 0
        project_attempts(kept_rows)
        tmp = path.with_name(path.name + ".compact")
        with tmp.open("w", encoding="utf-8") as handle:
            handle.writelines(kept_raws)
        os.replace(tmp, path)
    print(
        "lifecycle-compact: dropped %d event(s) across %d terminal attempt(s)"
        " older than %dd (kept %d)"
        % (len(rows) - len(kept_raws), len(drop), days, len(kept_raws))
    )
    return 0


def approval_projection(rows: Sequence[Dict[str, Any]], repo: Path, *, validate: bool = True) -> Dict[str, Dict[str, Any]]:
    projected: Dict[str, Dict[str, Any]] = {}
    events = set()
    repo_hash = hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest()
    for number, row in enumerate(rows, 1):
        if row.get("schema") != SCHEMA:
            raise OpsError("approval row %d has unsupported schema" % number)
        event_id = safe_id(str(row.get("event_id", "")), "approval event_id")
        if event_id in events:
            raise OpsError("duplicate approval event_id: %s" % event_id)
        events.add(event_id)
        approval_id = safe_id(str(row.get("approval_id", "")), "approval_id")
        parse_ts(str(row.get("ts", "")))
        version = row.get("version")
        if not isinstance(version, int) or isinstance(version, bool) or version < 1:
            raise OpsError("approval row %d has invalid version" % number)
        state = row.get("state")
        if state not in APPROVAL_STATES:
            raise OpsError("approval row %d has invalid state" % number)
        event_type = row.get("event_type")
        current = projected.get(approval_id)
        if event_type == "approval.requested":
            if current is not None or version != 1 or state != "requested":
                raise OpsError("invalid initial approval row for %s" % approval_id)
            if row.get("repo_hash") != repo_hash:
                raise OpsError("approval belongs to a different repository")
            parse_ts(str(row.get("expires_at", "")))
            values: Dict[str, Any] = {
                "attempt_id": safe_id(str(row.get("attempt_id", "")), "attempt", optional=True),
                "action": safe_id(str(row.get("action", "")), "action"),
                "object_id": safe_id(str(row.get("object_id", "")), "object_id"),
                "summary": bounded_text(str(row.get("summary", "")), "summary"),
                "base_sha": str(row.get("base_sha", "")),
                "patch_sha": str(row.get("patch_sha", "")),
                "task_id": safe_id(str(row.get("task_id", "")), "task_id", optional=True),
                "lease_id": safe_id(str(row.get("lease_id", "")), "lease_id", optional=True),
                "profile": safe_id(str(row.get("profile", "trusted-local")), "profile"),
                "parameters": row.get("parameters", {}),
            }
            if values["base_sha"] and not re.fullmatch(r"[0-9a-f]{40,64}", values["base_sha"]):
                raise OpsError("approval row %d has invalid base_sha" % number)
            if values["patch_sha"] and not HEX64_RE.fullmatch(values["patch_sha"]):
                raise OpsError("approval row %d has invalid patch_sha" % number)
            parameters = values["parameters"]
            if not isinstance(parameters, dict) or len(parameters) > 32:
                raise OpsError("approval row %d has invalid parameters" % number)
            for key, item in parameters.items():
                safe_id(str(key), "parameter key")
                if isinstance(item, str):
                    bounded_text(item, "parameter value", 160)
                elif isinstance(item, float) and not math.isfinite(item):
                    raise OpsError("approval row %d has non-finite parameter value" % number)
                elif item is not None and not isinstance(item, (bool, int, float)):
                    raise OpsError("approval row %d has invalid parameter value" % number)
            if row.get("action_digest") != approval_action_digest(repo, values):
                raise OpsError("approval row %d action digest does not match" % number)
            current = {
                "schema": SCHEMA,
                "approval_id": approval_id,
                "version": 1,
                "state": "requested",
                "attempt_id": row.get("attempt_id", ""),
                "action": row.get("action", ""),
                "object_id": row.get("object_id", ""),
                "summary": row.get("summary", ""),
                "action_digest": row.get("action_digest", ""),
                "base_sha": row.get("base_sha", ""),
                "patch_sha": row.get("patch_sha", ""),
                "task_id": row.get("task_id", ""),
                "lease_id": row.get("lease_id", ""),
                "profile": row.get("profile", "trusted-local"),
                "parameters": dict(parameters),
                "requested_at": row["ts"],
                "expires_at": row["expires_at"],
                "updated_at": row["ts"],
            }
            projected[approval_id] = current
            continue
        if current is None:
            raise OpsError("approval event precedes request for %s" % approval_id)
        if version != current["version"] + 1:
            raise OpsError("approval %s version is %s, expected %s" % (
                approval_id, version, current["version"] + 1
            ))
        expected = row.get("expected_version")
        if expected != current["version"]:
            raise OpsError("approval %s expected_version does not match" % approval_id)
        allowed: Dict[str, Tuple[set, str]] = {
            "approval.approved": ({"requested"}, "approved"),
            "approval.denied": ({"requested"}, "denied"),
            "approval.expired": ({"requested", "approved"}, "expired"),
            "approval.consuming": ({"approved"}, "consuming"),
            "approval.consumed": ({"consuming"}, "consumed"),
            "approval.failed": ({"consuming"}, "failed"),
            "approval.interrupted": ({"consuming"}, "interrupted"),
        }
        if event_type not in allowed:
            raise OpsError("approval row %d has invalid event_type" % number)
        from_states, target = allowed[event_type]
        if current["state"] not in from_states or state != target:
            raise OpsError("illegal approval transition: %s -> %s" % (current["state"], state))
        current["state"] = state
        current["version"] = version
        current["updated_at"] = row["ts"]
        if event_type == "approval.interrupted":
            if row.get("outcome") != "unknown":
                raise OpsError("interrupted approval outcome must be unknown")
            current["outcome"] = "unknown"
        if row.get("actor"):
            current["decided_by"] = row["actor"]
        if row.get("changes") is not None:
            current["changes"] = row["changes"]
        # Grant material stays internal to this process and is stripped from
        # all user-facing projections before printing.
        if row.get("grant_id"):
            current["_grant_id"] = row["grant_id"]
        if row.get("grant_hash"):
            current["_grant_hash"] = row["grant_hash"]
    return projected


def public_approval(row: Dict[str, Any]) -> Dict[str, Any]:
    return {key: value for key, value in row.items() if not key.startswith("_")}


def approval_action_digest(repo: Path, values: Dict[str, Any]) -> str:
    canonical = {"repo_hash": hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest()}
    canonical.update(values)
    return hashlib.sha256(json_bytes(canonical)).hexdigest()


def parse_scalar_map(raw: str, label: str) -> Dict[str, Any]:
    if not raw:
        return {}
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise OpsError("%s is not valid JSON" % label) from exc
    if not isinstance(value, dict) or len(value) > 32:
        raise OpsError("%s must be an object with at most 32 fields" % label)
    clean: Dict[str, Any] = {}
    for key, item in value.items():
        key = safe_id(str(key), "%s key" % label)
        if isinstance(item, str):
            clean[key] = bounded_text(item, "%s value" % label, 160)
        elif isinstance(item, float) and not math.isfinite(item):
            raise OpsError("%s values must be finite" % label)
        elif item is None or isinstance(item, (bool, int, float)):
            clean[key] = item
        else:
            raise OpsError("%s values must be bounded scalars" % label)
    return clean


def approval_request(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = approval_path(repo, create=True)
    approval_id = "apr_" + uuid.uuid4().hex
    action = safe_id(args.action, "action")
    object_id = safe_id(args.object_id, "object_id")
    attempt_id = safe_id(args.attempt, "attempt", optional=True)
    task_id = safe_id(args.task_id, "task_id", optional=True)
    lease_id = safe_id(args.lease_id, "lease_id", optional=True)
    profile = safe_id(args.profile, "profile")
    summary = bounded_text(args.summary, "summary")
    parameters = parse_scalar_map(args.parameters_json, "parameters-json")
    if args.expires_in < 1 or args.expires_in > 604800:
        raise OpsError("expires-in must be between 1 and 604800 seconds")
    now = dt.datetime.now(dt.timezone.utc).replace(microsecond=0)
    expires = now + dt.timedelta(seconds=args.expires_in)
    base_sha = args.base_sha or git_head(repo)
    patch_sha = args.patch_sha
    if base_sha and not re.fullmatch(r"[0-9a-f]{40,64}", base_sha):
        raise OpsError("base-sha must be a Git object id")
    if patch_sha and not HEX64_RE.fullmatch(patch_sha):
        raise OpsError("patch-sha must be 64 lowercase hex characters")
    values = {
        "attempt_id": attempt_id,
        "action": action,
        "object_id": object_id,
        "summary": summary,
        "base_sha": base_sha,
        "patch_sha": patch_sha,
        "task_id": task_id,
        "lease_id": lease_id,
        "profile": profile,
        "parameters": parameters,
    }
    row: Dict[str, Any] = {
        "schema": SCHEMA,
        "event_id": "apevt_" + uuid.uuid4().hex,
        "ts": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "approval_id": approval_id,
        "version": 1,
        "event_type": "approval.requested",
        "state": "requested",
        "repo_hash": hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest(),
        "expires_at": expires.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "action_digest": approval_action_digest(repo, values),
    }
    row.update({key: value for key, value in values.items() if value})
    with file_lock(path):
        rows = read_rows(path)
        approval_projection(rows, repo)
        append_row(path, row, private=True)
    print(approval_id)
    return 0


def parse_changes(raw: str) -> Optional[Dict[str, Any]]:
    if not raw:
        return None
    return parse_scalar_map(raw, "changes-json")


def approval_decide(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = approval_path(repo)
    approval_id = safe_id(args.approval, "approval")
    actor = safe_id(args.actor, "actor")
    if args.expected_version < 1:
        raise OpsError("expected-version must be positive")
    with file_lock(path):
        rows = read_rows(path)
        projected = approval_projection(rows, repo)
        current = projected.get(approval_id)
        if current is None:
            raise OpsError("unknown approval: %s" % approval_id)
        if current["version"] != args.expected_version or current["state"] != "requested":
            raise OpsError("approval compare-and-set failed: expected requested version %d" % args.expected_version)
        if dt.datetime.now(dt.timezone.utc) >= parse_ts(current["expires_at"]):
            raise OpsError("approval expired")
        version = current["version"] + 1
        row: Dict[str, Any] = {
            "schema": SCHEMA,
            "event_id": "apevt_" + uuid.uuid4().hex,
            "ts": utc_now(),
            "approval_id": approval_id,
            "version": version,
            "expected_version": current["version"],
            "actor": actor,
        }
        if args.decision == "approve":
            grant_id = "grant_" + uuid.uuid4().hex
            proof = secrets.token_urlsafe(32)
            digest = hashlib.sha256((approval_id + ":" + proof).encode("utf-8")).hexdigest()
            row.update({
                "event_type": "approval.approved",
                "state": "approved",
                "grant_id": grant_id,
                "grant_hash": digest,
            })
            changes = parse_changes(args.changes_json)
            if changes is not None:
                row["changes"] = changes
            append_row(path, row, private=True)
            print(grant_id + "." + proof)
        else:
            row.update({"event_type": "approval.denied", "state": "denied"})
            append_row(path, row, private=True)
            print(approval_id)
    return 0


def split_grant(grant_value: str) -> Tuple[str, str]:
    if "." not in grant_value:
        raise OpsError("invalid approval grant token")
    grant_id, proof = grant_value.split(".", 1)
    safe_id(grant_id, "grant id")
    if len(proof) < 32 or len(proof) > 128 or not re.fullmatch(r"[A-Za-z0-9_-]+", proof):
        raise OpsError("invalid approval grant token")
    return grant_id, proof


def approval_begin_consume(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = approval_path(repo)
    approval_id = safe_id(args.approval, "approval")
    consumer = safe_id(args.consumer, "consumer")
    grant_id, proof = split_grant(args.grant)
    with file_lock(path):
        rows = read_rows(path)
        projected = approval_projection(rows, repo)
        current = projected.get(approval_id)
        if current is None:
            raise OpsError("unknown approval: %s" % approval_id)
        if current["version"] != args.expected_version or current["state"] != "approved":
            raise OpsError("approval compare-and-set failed: expected approved version %d" % args.expected_version)
        if dt.datetime.now(dt.timezone.utc) >= parse_ts(current["expires_at"]):
            raise OpsError("approval expired")
        digest = hashlib.sha256((approval_id + ":" + proof).encode("utf-8")).hexdigest()
        if current.get("_grant_id") != grant_id or not hmac.compare_digest(current.get("_grant_hash", ""), digest):
            raise OpsError("approval grant does not match this action")
        row = {
            "schema": SCHEMA,
            "event_id": "apevt_" + uuid.uuid4().hex,
            "ts": utc_now(),
            "approval_id": approval_id,
            "version": current["version"] + 1,
            "expected_version": current["version"],
            "event_type": "approval.consuming",
            "state": "consuming",
            "actor": consumer,
        }
        append_row(path, row, private=True)
    print(approval_id)
    return 0


def approval_finish_consume(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = approval_path(repo)
    approval_id = safe_id(args.approval, "approval")
    consumer = safe_id(args.consumer, "consumer")
    with file_lock(path):
        rows = read_rows(path)
        projected = approval_projection(rows, repo)
        current = projected.get(approval_id)
        if current is None:
            raise OpsError("unknown approval: %s" % approval_id)
        if current["version"] != args.expected_version or current["state"] != "consuming":
            raise OpsError("approval compare-and-set failed: expected consuming version %d" % args.expected_version)
        event_type = "approval.consumed" if args.result == "consumed" else "approval.failed"
        row = {
            "schema": SCHEMA,
            "event_id": "apevt_" + uuid.uuid4().hex,
            "ts": utc_now(),
            "approval_id": approval_id,
            "version": current["version"] + 1,
            "expected_version": current["version"],
            "event_type": event_type,
            "state": args.result,
            "actor": consumer,
        }
        append_row(path, row, private=True)
    print(approval_id)
    return 0


def approval_expire(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = approval_path(repo)
    expired: List[str] = []
    with file_lock(path):
        rows = read_rows(path)
        projected = approval_projection(rows, repo)
        now = dt.datetime.now(dt.timezone.utc)
        for current in projected.values():
            if current["state"] not in {"requested", "approved"} or now < parse_ts(current["expires_at"]):
                continue
            expired.append(current["approval_id"])
            if args.apply:
                append_row(path, {
                    "schema": SCHEMA,
                    "event_id": "apevt_" + uuid.uuid4().hex,
                    "ts": utc_now(),
                    "approval_id": current["approval_id"],
                    "version": current["version"] + 1,
                    "expected_version": current["version"],
                    "event_type": "approval.expired",
                    "state": "expired",
                }, private=True)
    if args.json:
        print_json(expired)
    else:
        for approval_id in expired:
            print(approval_id)
        if not expired:
            print("no expired approvals")
    return 0


def approval_reconcile(args: argparse.Namespace) -> int:
    """Find stale reservations and, only on request, mark outcome unknown."""
    if args.older_than_seconds < 1 or args.older_than_seconds > 604800:
        raise OpsError("older-than-seconds must be between 1 and 604800 seconds")
    repo = repo_root(args.repo)
    path = approval_path(repo)
    stale: List[Dict[str, Any]] = []
    with file_lock(path):
        rows = read_rows(path)
        projected = approval_projection(rows, repo)
        now = dt.datetime.now(dt.timezone.utc)
        for current in projected.values():
            if current["state"] != "consuming":
                continue
            age_seconds = max(0, int((now - parse_ts(current["updated_at"])).total_seconds()))
            if age_seconds < args.older_than_seconds:
                continue
            item = {
                "approval_id": current["approval_id"],
                "version": current["version"],
                "state": "consuming",
                "age_seconds": age_seconds,
                "outcome": "unknown",
            }
            # patch-land writes a durable intent before reserving the grant.
            # Its own recovery can inspect both that intent and the Git tree
            # and therefore determine consumed versus failed. Generic
            # reconciliation must not preempt that stronger recovery with an
            # irreversible unknown-outcome terminal state.
            if current["action"] == "patch-land":
                item["recovery"] = "patch-land"
                stale.append(item)
                continue
            stale.append(item)
            if args.apply:
                append_row(path, {
                    "schema": SCHEMA,
                    "event_id": "apevt_" + uuid.uuid4().hex,
                    "ts": utc_now(),
                    "approval_id": current["approval_id"],
                    "version": current["version"] + 1,
                    "expected_version": current["version"],
                    "event_type": "approval.interrupted",
                    "state": "interrupted",
                    "outcome": "unknown",
                    "actor": "approval-reconciler",
                }, private=True)
    if args.json:
        print_json(stale)
    else:
        for item in stale:
            if item.get("recovery") == "patch-land":
                suffix = " recovery=patch-land"
                if args.apply:
                    suffix += " -> deferred"
            else:
                suffix = " -> interrupted" if args.apply else ""
            print("%s consuming age=%ss outcome=unknown%s" % (
                item["approval_id"], item["age_seconds"], suffix
            ))
        if not stale:
            print("no stale consuming approvals")
    return 0


def load_approvals(repo: Path) -> Dict[str, Dict[str, Any]]:
    return approval_projection(read_rows(approval_path(repo)), repo)


def approval_show(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    current = load_approvals(repo).get(safe_id(args.approval, "approval"))
    if current is None:
        raise OpsError("unknown approval: %s" % args.approval)
    value = public_approval(current)
    if args.json:
        print_json(value)
    else:
        print("approval: %s" % value["approval_id"])
        print("state/version: %s/%s" % (value["state"], value["version"]))
        print("action/object: %s/%s" % (value["action"], value["object_id"]))
        print("expires: %s" % value["expires_at"])
    return 0


def approval_list(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    values = [public_approval(value) for value in load_approvals(repo).values()]
    values.sort(key=lambda item: item["updated_at"])
    if args.pending:
        values = [item for item in values if item["state"] in {"requested", "approved", "consuming"}]
    if args.state:
        values = [item for item in values if item["state"] == args.state]
    if args.json:
        print_json(values)
    elif not values:
        print("no approvals")
    else:
        for item in values:
            print("%-38s v%-3s %-12s %-18s %s" % (
                item["approval_id"], item["version"], item["state"], item["action"], item["summary"]
            ))
    return 0


def approval_validate(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    path = approval_path(repo)
    rows = read_rows(path)
    approval_projection(rows, repo)
    mode = path.stat().st_mode & 0o777 if path.exists() else 0o600
    if path.exists() and mode & 0o077:
        raise OpsError("approval store permissions are too broad: %03o" % mode)
    print("approvals: ok (%d events)" % len(rows))
    return 0


def approval_path_command(args: argparse.Namespace) -> int:
    print(str(approval_path(repo_root(args.repo))))
    return 0


def add_event_parser(subparsers: argparse._SubParsersAction) -> None:
    start = subparsers.add_parser("start", help="create a queued attempt")
    start.add_argument("--provider", default="local")
    start.add_argument("--tool", required=True)
    start.add_argument("--task-id", default="")
    start.add_argument("--run-id", default="")
    start.add_argument("--parent-attempt-id", default="")
    start.add_argument("--max-wall-seconds", type=int)
    start.add_argument("--max-tokens", type=int)
    start.add_argument("--max-cost-microusd", type=int)
    start.add_argument("--idempotency-key", default="")
    start.set_defaults(func=event_start)

    transition = subparsers.add_parser("transition", help="append a validated state transition")
    transition.add_argument("--attempt", required=True)
    transition.add_argument("--state", required=True)
    transition.add_argument("--reason-code", default="")
    transition.add_argument("--actor-kind", default="owner")
    transition.add_argument("--actor", default="agent-events")
    transition.add_argument("--idempotency-key", default="")
    transition.set_defaults(func=event_transition)

    heartbeat = subparsers.add_parser("heartbeat", help="append runner liveness")
    heartbeat.add_argument("--attempt", required=True)
    heartbeat.add_argument("--actor", default="attempt-runner")
    heartbeat.add_argument("--idempotency-key", default="")
    heartbeat.set_defaults(func=event_heartbeat)

    usage = subparsers.add_parser("usage", help="append measured usage deltas")
    usage.add_argument("--attempt", required=True)
    usage.add_argument("--tokens", type=int)
    usage.add_argument("--duration-ms", type=int)
    usage.add_argument("--cost-microusd", type=int)
    usage.add_argument("--actor", default="provider-telemetry")
    usage.add_argument("--idempotency-key", default="")
    usage.set_defaults(func=event_usage)

    show = subparsers.add_parser("show", help="project one attempt")
    show.add_argument("--attempt", required=True)
    show.add_argument("--json", action="store_true")
    show.set_defaults(func=event_show)

    listing = subparsers.add_parser("list", help="list projected attempts")
    listing.add_argument("--active", action="store_true")
    listing.add_argument("--state", choices=sorted(ALL_STATES))
    listing.add_argument("--limit", type=int, default=0)
    listing.add_argument("--json", action="store_true")
    listing.set_defaults(func=event_list)

    resume = subparsers.add_parser("resume", help="create a new attempt from a durable spec")
    resume.add_argument("--attempt", required=True)
    resume.set_defaults(func=event_resume)

    follow = subparsers.add_parser("follow", help="stream lifecycle events as JSONL")
    follow.add_argument("--after-event", default="")
    follow.add_argument("--limit", type=int, default=0)
    follow.add_argument("--timeout", type=float, default=-1)
    follow.add_argument("--interval", type=float, default=0.5)
    follow.set_defaults(func=event_follow)

    reconcile = subparsers.add_parser("reconcile", help="find stale live attempts")
    reconcile.add_argument("--stale-seconds", type=int, default=300)
    reconcile.add_argument("--apply", action="store_true")
    reconcile.add_argument("--json", action="store_true")
    reconcile.set_defaults(func=event_reconcile)

    validate = subparsers.add_parser("validate", help="validate the append-only stream")
    validate.set_defaults(func=event_validate)

    compact = subparsers.add_parser(
        "compact", help="drop long-terminal attempts' event streams (whole streams only)"
    )
    compact.add_argument("--days", type=int, default=14)
    compact.add_argument("--apply", action="store_true")
    compact.set_defaults(func=event_compact)


def add_approval_parser(subparsers: argparse._SubParsersAction) -> None:
    request = subparsers.add_parser("request", help="request one bounded privileged effect")
    request.add_argument("--attempt", default="")
    request.add_argument("--action", required=True)
    request.add_argument("--object-id", required=True)
    request.add_argument("--summary", required=True)
    request.add_argument("--expires-in", type=int, default=3600)
    request.add_argument("--base-sha", default="")
    request.add_argument("--patch-sha", default="")
    request.add_argument("--task-id", default="")
    request.add_argument("--lease-id", default="")
    request.add_argument("--profile", default="trusted-local")
    request.add_argument("--parameters-json", default="")
    request.set_defaults(func=approval_request)

    decide = subparsers.add_parser("decide", help="approve or deny with version CAS")
    decide.add_argument("--approval", required=True)
    decide.add_argument("--decision", choices=("approve", "deny", "reject"), required=True)
    decide.add_argument("--expected-version", type=int, required=True)
    decide.add_argument("--actor", required=True)
    decide.add_argument("--changes-json", default="")
    decide.set_defaults(func=approval_decide)

    begin = subparsers.add_parser("begin-consume", help="reserve a one-time grant before the effect")
    begin.add_argument("--approval", required=True)
    begin.add_argument("--token", dest="grant", required=True)
    begin.add_argument("--expected-version", type=int, required=True)
    begin.add_argument("--consumer", required=True)
    begin.set_defaults(func=approval_begin_consume)

    finish = subparsers.add_parser("finish-consume", help="terminalize a reserved effect")
    finish.add_argument("--approval", required=True)
    finish.add_argument("--expected-version", type=int, required=True)
    finish.add_argument("--result", choices=("consumed", "failed"), required=True)
    finish.add_argument("--consumer", required=True)
    finish.set_defaults(func=approval_finish_consume)

    expire = subparsers.add_parser("expire", help="find or append expired unreserved approvals")
    expire.add_argument("--apply", action="store_true", help="append terminal expiry rows (default: report only)")
    expire.add_argument("--json", action="store_true")
    expire.set_defaults(func=approval_expire)

    reconcile = subparsers.add_parser(
        "reconcile", help="find stale consuming approvals without guessing their outcome"
    )
    reconcile.add_argument(
        "--older-than-seconds", type=int, default=300,
        help="minimum reservation age, 1-604800 seconds (default: 300)",
    )
    reconcile.add_argument(
        "--apply", action="store_true",
        help="append interrupted/unknown rows (default: report only)",
    )
    reconcile.add_argument("--json", action="store_true")
    reconcile.set_defaults(func=approval_reconcile)

    show = subparsers.add_parser("show", help="show one approval projection")
    show.add_argument("--approval", required=True)
    show.add_argument("--json", action="store_true")
    show.set_defaults(func=approval_show)

    listing = subparsers.add_parser("list", help="list approvals")
    listing.add_argument("--pending", action="store_true")
    listing.add_argument("--state", choices=sorted(APPROVAL_STATES))
    listing.add_argument("--json", action="store_true")
    listing.set_defaults(func=approval_list)

    validate = subparsers.add_parser("validate", help="validate approval state and permissions")
    validate.set_defaults(func=approval_validate)

    path = subparsers.add_parser("path", help="print this repository's private approval store")
    path.set_defaults(func=approval_path_command)


def build_parser(app: str) -> argparse.ArgumentParser:
    if app == "events":
        parser = argparse.ArgumentParser(prog="agent-events.sh", description="Append and project durable agent lifecycle events.")
    else:
        parser = argparse.ArgumentParser(prog="approval-inbox.sh", description="Durable compare-and-set approvals outside worker-writable state.")
    parser.add_argument("--repo", default=".")
    subparsers = parser.add_subparsers(dest="command", required=True)
    if app == "events":
        add_event_parser(subparsers)
    else:
        add_approval_parser(subparsers)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args_list = list(sys.argv[1:] if argv is None else argv)
    if not args_list or args_list[0] not in {"events", "approvals"}:
        raise OpsError("internal app selector must be events or approvals")
    app = args_list.pop(0)
    parser = build_parser(app)
    args = parser.parse_args(args_list)
    # Treat reject as the user-facing synonym while keeping one durable state.
    if getattr(args, "decision", None) == "reject":
        args.decision = "deny"
    return int(args.func(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except OpsError as exc:
        print("error: %s" % exc, file=sys.stderr)
        raise SystemExit(2)
