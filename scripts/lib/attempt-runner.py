#!/usr/bin/env python3
"""Bounded, crash-observable process supervision for OMS lifecycle attempts."""

from __future__ import annotations

import argparse
import collections
import importlib.util
import json
import os
import re
import select
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple


def load_events_module() -> Any:
    path = Path(__file__).with_name("agent-events.py")
    spec = importlib.util.spec_from_file_location("oms_agent_events", str(path))
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load %s" % path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ae = load_events_module()

DEFAULT_MAX_LOG_BYTES = 8 * 1024 * 1024
MIN_MAX_LOG_BYTES = 1024
MAX_MAX_LOG_BYTES = 100 * 1024 * 1024
ORPHAN_JOB_GRACE_SECONDS = 10


def log_truncation_marker(dropped_bytes: int) -> bytes:
    return (
        "\n[oms: output truncated at log byte limit; %d bytes dropped]\n"
        % dropped_bytes
    ).encode("ascii")


def launch_supervised_process(argv: Sequence[str], cwd: str, env: Dict[str, str]) -> subprocess.Popen[Any]:
    return subprocess.Popen(list(argv), cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=True)


def runtime_root(repo: Path) -> Path:
    state_home = os.environ.get("XDG_STATE_HOME")
    if state_home:
        base = Path(state_home).expanduser()
    else:
        base = Path.home() / ".local" / "state"
    digest = ae.hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest()
    root = base / "oh-my-setting" / "runtime" / digest
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        root.chmod(0o700)
    except OSError:
        pass
    return root


def queue_lock_path(repo: Path) -> Path:
    return runtime_root(repo) / "queue.state"


def job_path(repo: Path, attempt_id: str) -> Path:
    return runtime_root(repo) / "jobs" / (attempt_id + ".json")


def log_path(repo: Path, attempt_id: str) -> Path:
    return runtime_root(repo) / "logs" / (attempt_id + ".log")


class BoundedLogCapture:
    """Drain a worker pipe while keeping the durable log strictly bounded."""

    def __init__(self, path: Path, max_bytes: int) -> None:
        self.path = path
        self.max_bytes = max_bytes
        self.bytes_written = 0
        self.total_bytes = 0
        self.truncated = False
        self.error: Optional[BaseException] = None
        self.stream: Any = None
        self.thread: Optional[threading.Thread] = None
        self.stop_requested = threading.Event()
        self.detached = False
        self.tail_limit = (max_bytes + 1) // 2
        self.tail_chunks: "collections.deque[bytes]" = collections.deque()
        self.tail_bytes = 0

    @staticmethod
    def _write_all(fd: int, data: bytes) -> None:
        offset = 0
        while offset < len(data):
            written = os.write(fd, data[offset:])
            if written <= 0:
                raise OSError("short write while capturing worker output")
            offset += written

    def start(self, stream: Any) -> None:
        self.stream = stream
        self.thread = threading.Thread(
            target=self._run,
            name="oms-log-capture",
            daemon=True,
        )
        self.thread.start()

    def _remember_tail(self, chunk: bytes) -> None:
        self.tail_chunks.append(chunk)
        self.tail_bytes += len(chunk)
        excess = self.tail_bytes - self.tail_limit
        while excess > 0:
            first = self.tail_chunks[0]
            if len(first) <= excess:
                self.tail_chunks.popleft()
                self.tail_bytes -= len(first)
                excess -= len(first)
            else:
                self.tail_chunks[0] = first[excess:]
                self.tail_bytes -= excess
                excess = 0

    def _truncated_layout(self) -> Tuple[int, bytes, bytes]:
        tail = b"".join(self.tail_chunks)
        # A truncated capture always ends with a newline, as the old
        # marker-last layout guaranteed: attach() re-adds one to a final
        # line that lacks it, and that single byte pushed the read past the
        # byte cap the caller was promised.
        terminal = b"" if tail.endswith(b"\n") else b"\n"
        head_bytes = 0
        while True:
            dropped_bytes = self.total_bytes - head_bytes - len(tail)
            marker = log_truncation_marker(dropped_bytes)
            next_head_bytes = max(
                0, self.max_bytes - len(tail) - len(terminal) - len(marker)
            )
            next_head_bytes = min(self.bytes_written, next_head_bytes)
            if next_head_bytes == head_bytes:
                return head_bytes, marker, tail + terminal
            head_bytes = next_head_bytes

    def _run(self) -> None:
        fd = -1
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            fd = os.open(
                str(self.path),
                os.O_WRONLY | os.O_CREAT | os.O_TRUNC,
                0o600,
            )
        except BaseException as exc:
            self.error = exc
        try:
            while True:
                if self.stop_requested.is_set():
                    break
                try:
                    ready, _, _ = select.select([self.stream.fileno()], [], [], 0.1)
                except (OSError, ValueError) as exc:
                    if self.stop_requested.is_set():
                        break
                    raise exc
                if not ready:
                    continue
                chunk = os.read(self.stream.fileno(), 65536)
                if not chunk:
                    break
                self.total_bytes += len(chunk)
                self._remember_tail(chunk)
                if fd < 0 or self.error is not None:
                    continue
                remaining = self.max_bytes - self.bytes_written
                if remaining > 0:
                    selected = chunk[:remaining]
                    try:
                        self._write_all(fd, selected)
                    except BaseException as exc:
                        self.error = exc
                        continue
                    self.bytes_written += len(selected)
                if len(chunk) > remaining:
                    self.truncated = True
        except BaseException as exc:
            if self.error is None:
                self.error = exc
        finally:
            if fd >= 0:
                try:
                    if self.total_bytes > self.max_bytes and self.error is None:
                        kept, marker, tail = self._truncated_layout()
                        os.ftruncate(fd, kept)
                        os.lseek(fd, 0, os.SEEK_END)
                        self._write_all(fd, marker)
                        self._write_all(fd, tail)
                        self.bytes_written = kept + len(marker) + len(tail)
                    os.fsync(fd)
                except BaseException as exc:
                    if self.error is None:
                        self.error = exc
                finally:
                    os.close(fd)
            try:
                self.stream.close()
            except (AttributeError, OSError):
                pass

    def wait(self) -> None:
        if self.thread is None:
            return
        # Give an ordinary closed pipe a short drain window, then stop polling.
        # A child that starts a new session can retain the inherited write end;
        # it is outside our signal scope and must not hold attempt completion.
        self.thread.join(timeout=0.25)
        if self.thread.is_alive():
            self.truncated = True
            self.stop_requested.set()
            self.thread.join(timeout=0.5)
        if self.thread.is_alive():
            # Closing a buffered pipe from this thread can itself block behind
            # the reader lock. Leave the daemon reader to process teardown and
            # publish the bounded log snapshot instead.
            self.detached = True
            return
        if self.error is not None:
            raise ae.OpsError("could not persist worker output: %s" % self.error)


def _finish_job(job: Dict[str, Any], runtime_state: str, **extra: Any) -> None:
    job.update({
        "runtime_state": runtime_state,
        "pid": 0,
        "pid_identity": "",
        "runner_pid": 0,
        "runner_pid_identity": "",
        "finished_at": ae.utc_now(),
        **extra,
    })


def load_job(repo: Path, attempt_id: str) -> Dict[str, Any]:
    path = job_path(repo, attempt_id)
    try:
        row = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ae.OpsError("no supervised job for attempt: %s" % attempt_id) from exc
    if not isinstance(row, dict) or row.get("schema") != 1 or row.get("attempt_id") != attempt_id:
        raise ae.OpsError("invalid supervised job: %s" % attempt_id)
    return row


def save_job(repo: Path, row: Dict[str, Any]) -> None:
    row["updated_at"] = ae.utc_now()
    ae.write_json_atomic(job_path(repo, row["attempt_id"]), row, 0o600)


def save_new_supervisor_job(repo: Path, row: Dict[str, Any]) -> None:
    """Publish a new runtime job only while its lifecycle intent is queued."""
    def semantics(value: Dict[str, Any]) -> Dict[str, Any]:
        return {key: item for key, item in value.items() if key != "updated_at"}

    attempt_id = str(row.get("attempt_id") or "")
    if not attempt_id:
        raise ae.OpsError("new supervisor job has no attempt id")
    with ae.file_lock(queue_lock_path(repo)):
        # Serialize the queued-state check with lifecycle transitions that obey
        # the same queue -> event lock order (notably orphan reconciliation).
        with ae.file_lock(ae.event_path(repo)):
            rows = ae.read_rows(ae.event_path(repo))
            attempt = ae.project_attempts(rows).get(attempt_id)
            if attempt is None or attempt.get("tool") != "agent-supervisor":
                raise ae.OpsError(
                    "runtime job is not owned by an agent-supervisor attempt"
                )
            if attempt.get("state") != "queued":
                raise ae.OpsError(
                    "runtime job cannot publish after lifecycle state %s"
                    % attempt.get("state", "unknown")
                )
            path = job_path(repo, attempt_id)
            if path.exists():
                existing = load_job(repo, attempt_id)
                if semantics(existing) == semantics(row):
                    return
                raise ae.OpsError(
                    "runtime job already exists with different supervisor intent"
                )
            try:
                save_job(repo, row)
            except (OSError, ValueError, ae.OpsError):
                # An atomic rename may have succeeded before a durability call
                # reported failure. Accept only the exact durable intent; a
                # missing, corrupt, or different record remains an error.
                try:
                    existing = load_job(repo, attempt_id)
                except ae.OpsError:
                    pass
                else:
                    if semantics(existing) == semantics(row):
                        return
                raise


def all_jobs(repo: Path, *, strict: bool = False) -> List[Dict[str, Any]]:
    directory = runtime_root(repo) / "jobs"
    if not directory.exists():
        return []
    rows: List[Dict[str, Any]] = []
    for path in sorted(directory.glob("att_*.json")):
        try:
            row = json.loads(path.read_text(encoding="utf-8"))
            if (
                not isinstance(row, dict)
                or row.get("schema") != 1
                or row.get("attempt_id") != path.stem
                or not isinstance(row.get("runtime_state"), str)
            ):
                raise ValueError("unsupported job schema")
            rows.append(row)
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            if strict:
                raise ae.OpsError(
                    "cannot safely read runtime job %s; run reconcile after repairing the record"
                    % path.name
                ) from exc
    return rows


def same_git_repository(repo: Path, cwd: Path) -> bool:
    def common(path: Path) -> str:
        try:
            value = subprocess.check_output(
                ["git", "-C", str(path), "rev-parse", "--git-common-dir"],
                stderr=subprocess.DEVNULL,
            ).decode("utf-8", "replace").strip().rstrip("\r")
        except (OSError, subprocess.CalledProcessError):
            return ""
        candidate = Path(value)
        if not candidate.is_absolute():
            candidate = path / candidate
        return str(candidate.resolve())

    left = common(repo)
    right = common(cwd)
    return bool(left and right and left == right)


def command_digest(argv: Sequence[str]) -> str:
    return ae.hashlib.sha256(ae.json_bytes(list(argv))).hexdigest()


def validate_command(argv: Sequence[str]) -> List[str]:
    values = list(argv)
    if values and values[0] == "--":
        values = values[1:]
    if not values or not values[0]:
        raise ae.OpsError("a command is required after --")
    if len(values) > 256 or sum(len(value) for value in values) > 32768:
        raise ae.OpsError("command is too large for a durable supervised job")
    sensitive = re.compile(
        r"(?i)(api[_-]?key|access[_-]?token|secret|password|private[_-]?key)\s*[:=]|"
        r"AKIA[0-9A-Z]{16}|-----BE" r"GIN [A-Z ]+PRIVATE " r"KEY-----"
    )
    for value in values:
        if "\x00" in value or sensitive.search(value):
            raise ae.OpsError("command contains sensitive-looking material; pass credentials through a scoped runtime instead")
    return values


def transition(repo: Path, attempt_id: str, state: str, reason: str = "", key: str = "") -> Dict[str, Any]:
    event = ae.new_event(
        attempt_id,
        0,
        "attempt.state_changed",
        from_state=None,
        to_state=state,
        reason_code=reason,
        actor={"kind": "supervisor", "name": "agent-supervisor"},
        idempotency_key=key,
    )
    return ae.append_lifecycle(repo, event)


def heartbeat(repo: Path, attempt_id: str, key: str) -> None:
    event = ae.new_event(
        attempt_id,
        0,
        "attempt.heartbeat",
        actor={"kind": "runner", "name": "attempt-runner"},
        idempotency_key=key,
    )
    ae.append_lifecycle(repo, event)


def usage(repo: Path, attempt_id: str, values: Dict[str, int], key: str) -> None:
    event = ae.new_event(
        attempt_id,
        0,
        "attempt.usage",
        usage=values,
        actor={"kind": "runner", "name": "attempt-runner"},
        idempotency_key=key,
    )
    ae.append_lifecycle(repo, event)


def public_job(repo: Path, row: Dict[str, Any]) -> Dict[str, Any]:
    _, attempts = ae.load_projection(repo)
    projected = attempts.get(row["attempt_id"], {})
    pid = int(row.get("pid") or 0)
    return {
        "schema": 1,
        "attempt_id": row["attempt_id"],
        "state": projected.get("state", "unknown"),
        "reason_code": projected.get("reason_code", ""),
        "runtime_state": row.get("runtime_state", "unknown"),
        "provider": row.get("provider", ""),
        "profile": row.get("profile", ""),
        "completion_state": row.get("completion_state", "review"),
        "command_sha256": row.get("command_sha256", ""),
        "exit_code": row.get("exit_code"),
        "max_log_bytes": row.get("max_log_bytes", DEFAULT_MAX_LOG_BYTES),
        "log_bytes": row.get("log_bytes", 0),
        "log_truncated": bool(row.get("log_truncated", False)),
        "pid_alive": process_matches(pid, str(row.get("pid_identity") or "")),
        "created_at": row.get("created_at", ""),
        "updated_at": row.get("updated_at", ""),
        "usage": projected.get("usage", {}),
        "usage_reports": projected.get("usage_reports", {}),
        "budget": projected.get("budget", {}),
        "parent_attempt_id": projected.get("parent_attempt_id", ""),
        # These are explicit machine-readable limits, not inferred promises.
        # Resume always creates a new child attempt. The local runner owns one
        # POSIX process group; this is not a filesystem or network sandbox.
        "resume_mode": "child_attempt",
        "input_resume_supported": False,
        "approval_scope": "patch_land_only",
        "sandbox": False,
        "supervision_scope": "process_group",
    }


def submit(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    argv = validate_command(args.command_argv)
    cwd = Path(args.cwd).expanduser().resolve() if args.cwd else repo
    if not cwd.is_dir() or not same_git_repository(repo, cwd):
        raise ae.OpsError("cwd must be this repository or one of its Git worktrees")
    if args.profile != "trusted-local":
        raise ae.OpsError("agent-supervisor currently accepts trusted-local; use oms runtime backend check for other backends")
    max_log_bytes = getattr(args, "max_log_bytes", DEFAULT_MAX_LOG_BYTES)
    if (
        isinstance(max_log_bytes, bool)
        or not isinstance(max_log_bytes, int)
        or max_log_bytes < MIN_MAX_LOG_BYTES
        or max_log_bytes > MAX_MAX_LOG_BYTES
    ):
        raise ae.OpsError(
            "max-log-bytes must be between %d and %d"
            % (MIN_MAX_LOG_BYTES, MAX_MAX_LOG_BYTES)
        )
    budget = {
        key: value
        for key, value in {
            "max_wall_seconds": args.max_wall_seconds,
            "max_tokens": args.max_tokens,
            "max_cost_microusd": args.max_cost_microusd,
        }.items()
        if value is not None
    }
    attempt_id = ae.create_attempt(
        repo,
        provider=args.provider,
        tool="agent-supervisor",
        task_id=args.task_id,
        run_id=args.run_id,
        budget=budget,
        refs={"command_sha256": command_digest(argv)},
    )
    try:
        spec_path = ae.attempt_spec_path(repo, attempt_id)
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
        spec.update({
            "profile": args.profile,
            "completion_state": args.completion_state,
            "command_sha256": command_digest(argv),
        })
        ae.write_json_atomic(spec_path, spec)
        now = ae.utc_now()
        row: Dict[str, Any] = {
            "schema": 1,
            "attempt_id": attempt_id,
            "provider": ae.safe_id(args.provider, "provider"),
            "profile": args.profile,
            "completion_state": args.completion_state,
            "task_id": args.task_id,
            "argv": argv,
            "cwd": str(cwd),
            "command_sha256": command_digest(argv),
            "budget": budget,
            "runtime_state": "queued",
            "queue_order": time.time_ns(),
            "created_at": now,
            "updated_at": now,
            "exit_code": None,
            "max_log_bytes": max_log_bytes,
            "log_bytes": 0,
            "log_truncated": False,
            "pid": 0,
            "pid_identity": "",
            "runner_pid": 0,
            "runner_pid_identity": "",
        }
        save_new_supervisor_job(repo, row)
    except (OSError, ValueError, ae.OpsError):
        try:
            transition(repo, attempt_id, "blocked", "submit_incomplete", "submit-incomplete")
        except (OSError, ValueError, ae.OpsError):
            pass
        raise
    print(attempt_id)
    return 0


def terminate_process_group(
    pid: int, expected_identity: str, *, force: bool = False
) -> bool:
    """Signal a recorded tree only while its leader identity still matches."""
    if not recorded_tree_matches(pid, expected_identity):
        return False
    try:
        os.killpg(pid, signal.SIGKILL if force else signal.SIGTERM)
    except ProcessLookupError:
        pass
    except PermissionError:
        raise ae.OpsError("cannot signal supervised process group")
    return True


def process_identity(pid: int) -> str:
    """Return a start identity so stale PID records never target a reuse."""
    if pid <= 0:
        return ""
    try:
        tail = ae.proc_stat_fields(pid)
        start_ticks = tail[19]
        boot_id = Path("/proc/sys/kernel/random/boot_id").read_text(
            encoding="utf-8", errors="replace"
        ).strip()
        if start_ticks.isdigit() and boot_id:
            return ae.hashlib.sha256((boot_id + ":" + start_ticks).encode("utf-8")).hexdigest()
    except (OSError, IndexError):
        pass
    try:
        result = subprocess.run(
            ["ps", "-o", "lstart=", "-p", str(pid)],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    value = result.stdout.strip().rstrip(b"\r")
    if result.returncode != 0 or not value:
        return ""
    return ae.hashlib.sha256(value).hexdigest()


def process_matches(pid: int, expected: str) -> bool:
    if pid <= 0 or not expected:
        return False
    current = process_identity(pid)
    return bool(current and current == expected)


def pid_is_zombie(pid: int) -> bool:
    """A zombie keeps its identity for kill fencing but can never run again."""
    if pid <= 0:
        return False
    try:
        tail = ae.proc_stat_fields(pid)
        if tail:
            return tail[0] == "Z"
    except OSError:
        pass
    try:
        result = subprocess.run(
            ["ps", "-o", "stat=", "-p", str(pid)],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    state = result.stdout.decode("ascii", "replace").strip()
    return result.returncode == 0 and state.startswith("Z")


def recorded_tree_matches(pid: int, expected: str) -> bool:
    """Validate both leader start identity and the group-leader contract."""
    if not process_matches(pid, expected):
        return False
    try:
        return os.getpgid(pid) == pid
    except (OSError, AttributeError):
        return False


def leader_exited_unreaped(process: subprocess.Popen[Any], expected: str) -> bool:
    """Observe POSIX leader exit without reaping away its identity token."""
    if process.returncode is not None:
        return True
    try:
        tail = ae.proc_stat_fields(process.pid)
        if tail:
            return tail[0] == "Z"
    except OSError:
        pass
    try:
        result = subprocess.run(
            ["ps", "-o", "stat=", "-p", str(process.pid)],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
        )
    except (OSError, subprocess.TimeoutExpired):
        return process.poll() is not None
    state = result.stdout.decode("ascii", "replace").strip().rstrip("\r")
    if result.returncode != 0 or not state:
        return True
    if expected and not process_matches(process.pid, expected):
        return True
    return state.startswith("Z")


def _group_has_runnable_member(pgid: int) -> Optional[bool]:
    """True/False when membership is observable, None when it is not."""
    proc = Path("/proc")
    expected = str(pgid)
    scanned = False
    if proc.is_dir():
        for entry in proc.iterdir():
            if not entry.name.isdigit():
                continue
            try:
                tail = ae.proc_stat_fields(entry.name)
            except OSError:
                continue
            if len(tail) < 3:
                continue
            scanned = True
            if tail[2] == expected and tail[0] != "Z":
                return True
        if scanned:
            return False
    try:
        result = subprocess.run(
            ["ps", "-axo", "pgid=,stat="],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=3,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    for line in result.stdout.decode("ascii", "replace").splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == expected and not parts[1].startswith("Z"):
            return True
    return False


def process_group_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # killpg(0) succeeds while any member exists, including a zombie whose
    # reaper has not collected it yet. A whole-tree subreaper (autopilot
    # receipt supervise) defers adopted reaping until its phase exits, so an
    # orphaned member's zombie can outlive every kill this module sends; only
    # a member that can still run keeps the group alive in fact.
    running = _group_has_runnable_member(pid)
    if running is None:
        return True
    return running


def stop_recorded_tree(pid: int, expected_identity: str) -> bool:
    """Best-effort cleanup for reconcile, with a bounded graceful window."""
    if pid <= 0:
        return True
    if ae.pid_alive(pid) and not recorded_tree_matches(pid, expected_identity):
        return False
    if not process_group_alive(pid):
        return True
    if not recorded_tree_matches(pid, expected_identity):
        return False
    if not terminate_process_group(pid, expected_identity):
        return False
    deadline = time.monotonic() + 2
    while process_group_alive(pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    if process_group_alive(pid):
        if not terminate_process_group(pid, expected_identity, force=True):
            return False
    deadline = time.monotonic() + 3
    while process_group_alive(pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    return not process_group_alive(pid)


def stop_process_tree(
    process: subprocess.Popen[Any],
    expected_identity: str,
) -> int:
    """Terminate and reap the leader, then kill surviving process-group members."""
    if process.returncode is not None:
        return int(process.returncode)
    if process_group_alive(process.pid):
        if not terminate_process_group(process.pid, expected_identity):
            raise ae.OpsError("supervised process identity changed before cleanup")
        deadline = time.monotonic() + 2
        while (
            not leader_exited_unreaped(process, expected_identity)
            and time.monotonic() < deadline
        ):
            time.sleep(0.05)
        if process_group_alive(process.pid):
            if not terminate_process_group(process.pid, expected_identity, force=True):
                raise ae.OpsError("supervised process identity changed before forced cleanup")
    try:
        exit_code = process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        raise ae.OpsError("supervised process leader did not terminate")
    # Never signal the numeric group after reaping its identity-bearing leader.
    # The pre-reap forced signal above owns same-group descendants; a process
    # that deliberately changed session is outside trusted-local supervision.
    deadline = time.monotonic() + 1
    while process_group_alive(process.pid) and time.monotonic() < deadline:
        time.sleep(0.02)
    if process_group_alive(process.pid):
        raise ae.OpsError("supervised process group did not terminate")
    return int(exit_code)


def runner(repo: Path, attempt_id: str) -> int:
    lock_target = queue_lock_path(repo)
    cancelled_before_launch = False
    hard_budget_unsupported = False
    with ae.file_lock(lock_target):
        job = load_job(repo, attempt_id)
        if job.get("runtime_state") == "cancel_requested":
            cancelled_before_launch = True
            _finish_job(job, "cancelled", exit_code=None)
            save_job(repo, job)
        elif job.get("runtime_state") not in {"queued", "launching"}:
            return 0
        elif any(
            name in job.get("budget", {})
            for name in ("max_tokens", "max_cost_microusd")
        ):
            # trusted-local exposes no authenticated provider-native meter or
            # limiter. Starting the worker and checking self-reported events
            # afterward would already have spent the budget, so refuse before
            # Popen instead.
            hard_budget_unsupported = True
            _finish_job(job, "finished", exit_code=78)
            save_job(repo, job)
        else:
            job["runtime_state"] = "starting"
            job["runner_pid"] = os.getpid()
            job["runner_pid_identity"] = process_identity(os.getpid())
            save_job(repo, job)
    if cancelled_before_launch:
        transition(
            repo,
            attempt_id,
            "cancelled",
            "cancelled_by_owner",
            "runner-cancelled-before-launch",
        )
        return 130
    if hard_budget_unsupported:
        transition(
            repo,
            attempt_id,
            "failed",
            "hard_budget_unsupported",
            "runner-hard-budget-unsupported",
        )
        return 78
    transition(repo, attempt_id, "starting", key="runner-starting")

    output = log_path(repo, attempt_id)
    max_log_bytes = int(job.get("max_log_bytes", DEFAULT_MAX_LOG_BYTES))
    capture = BoundedLogCapture(output, max_log_bytes)
    started = time.monotonic()
    process: Optional[subprocess.Popen[Any]] = None
    process_identity_value = ""
    try:
        env = os.environ.copy()
        env.update({
            "OMS_ATTEMPT_ID": attempt_id,
            # Provider wrappers contribute token/cost telemetry to this outer
            # attempt, while the supervisor remains the single wall-clock
            # owner. Without this marker both layers report the same elapsed
            # interval and projected duration is double-counted.
            "OMS_ATTEMPT_SUPERVISED": "1",
            "OMS_EXECUTION_PROFILE": job.get("profile", "trusted-local"),
        })
        process = launch_supervised_process(
            job["argv"], job["cwd"], env
        )
    except (OSError, ValueError, ae.OpsError) as exc:
        raw_msg = ("launch failed: %s\n" % exc).encode("utf-8", "replace")
        output.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(str(output), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        try:
            BoundedLogCapture._write_all(fd, raw_msg[:max_log_bytes])
            os.fsync(fd)
        finally:
            os.close(fd)
        with ae.file_lock(lock_target):
            job = load_job(repo, attempt_id)
            _finish_job(
                job,
                "finished",
                exit_code=127,
                log_bytes=len(raw_msg[:max_log_bytes]),
                log_truncated=len(raw_msg) > max_log_bytes,
            )
            save_job(repo, job)
        transition(repo, attempt_id, "failed", "launch_failed", "runner-launch-failed")
        return 127

    if process is None:
        raise ae.OpsError("supervised process was not created")
    try:
        if process.stdout is None:
            raise ae.OpsError("supervised process output pipe was not created")
        process_identity_value = process_identity(process.pid)
        if not process_identity_value and process.poll() is None:
            raise ae.OpsError("could not establish supervised process identity")
        capture.start(process.stdout)
        cancelled = False
        with ae.file_lock(lock_target):
            job = load_job(repo, attempt_id)
            if job.get("runtime_state") == "cancel_requested":
                cancelled = True
            elif job.get("runtime_state") != "starting":
                raise ae.OpsError(
                    "attempt changed state during launch: %s" % job.get("runtime_state")
                )
            job["pid"] = process.pid
            job["pid_identity"] = process_identity_value
            if not cancelled:
                job["runtime_state"] = "running"
            save_job(repo, job)
        if not cancelled:
            transition(repo, attempt_id, "working", key="runner-working")

        max_wall = job.get("budget", {}).get("max_wall_seconds")
        heartbeat_seconds = 10.0
        next_heartbeat = time.monotonic() + heartbeat_seconds
        timed_out = False
        identity_failed = False
        if cancelled:
            exit_code = stop_process_tree(
                process, process_identity_value
            )
        else:
            while not leader_exited_unreaped(process, process_identity_value):
                now = time.monotonic()
                if max_wall is not None and now - started >= int(max_wall):
                    timed_out = True
                    break
                try:
                    current_job = load_job(repo, attempt_id)
                    current_runtime = current_job.get("runtime_state")
                    cancelled = current_runtime == "cancel_requested"
                    identity_failed = current_runtime == "interrupted"
                except ae.OpsError:
                    cancelled = True
                if cancelled or identity_failed:
                    break
                if now >= next_heartbeat:
                    heartbeat(repo, attempt_id, "heartbeat-%d" % int(now // heartbeat_seconds))
                    next_heartbeat = now + heartbeat_seconds
                time.sleep(0.1)
            # This is also required after a zero-exit leader: background
            # members that remain in the owned process group cannot outlive
            # the attempt. A process that deliberately starts a new session is
            # outside trusted-local supervision and requires OS containment.
            exit_code = stop_process_tree(
                process, process_identity_value
            )
        capture.wait()

        duration_ms = max(0, int((time.monotonic() - started) * 1000))
        usage(repo, attempt_id, {"duration_ms": duration_ms}, "runner-duration")

        with ae.file_lock(lock_target):
            job = load_job(repo, attempt_id)
            if job.get("runtime_state") == "cancel_requested":
                cancelled = True
            _finish_job(
                job,
                "interrupted"
                if identity_failed
                else ("cancelled" if cancelled else "finished"),
                exit_code=int(exit_code),
                log_bytes=capture.bytes_written,
                log_truncated=capture.truncated,
            )
            save_job(repo, job)

        if timed_out:
            transition(repo, attempt_id, "timed_out", "wall_budget_exceeded", "runner-timed-out")
            return 124
        if cancelled:
            transition(repo, attempt_id, "cancelled", "cancelled_by_owner", "runner-cancelled")
            return 130
        if identity_failed:
            _, attempts = ae.load_projection(repo)
            if attempts.get(attempt_id, {}).get("state") in ae.NONTERMINAL_STATES:
                transition(
                    repo,
                    attempt_id,
                    "failed",
                    "process_identity_mismatch",
                    "process-identity-mismatch",
                )
            return 78
        if capture.truncated:
            # Completion stands (the exit code is the contract), but the flag
            # must be louder than a ledger field no reader consults: say it
            # where the operator and logs will see it. Two distinct causes:
            # a byte-cap cut keeps head and tail (the marker carries the
            # dropped count); an undrained pipe stops the capture early, so
            # the tail — where a final report or verdict lands — may be gone.
            if capture.total_bytes > capture.max_bytes and capture.error is None:
                print(
                    "attempt %s: log hit the %d-byte cap (%d bytes produced);"
                    " head and tail are kept, the middle was dropped — the"
                    " in-log marker carries the dropped byte count"
                    % (attempt_id, capture.max_bytes, capture.total_bytes),
                    file=sys.stderr,
                )
            else:
                print(
                    "attempt %s: log capture stopped at %d bytes; the tail"
                    " (where a final report lands) may be missing — judge by"
                    " recorded state, not the log"
                    % (attempt_id, capture.bytes_written),
                    file=sys.stderr,
                )
        if exit_code != 0:
            transition(repo, attempt_id, "failed", "command_failed", "runner-command-failed")
            return int(exit_code)

        transition(repo, attempt_id, "verifying", key="runner-verifying")
        _, attempts = ae.load_projection(repo)
        current = attempts[attempt_id]
        budget = current.get("budget", {})
        if any(name in budget for name in ("max_tokens", "max_cost_microusd")):
            # Defensive convergence for a runtime record whose budget changed
            # after preflight. Advisory reports remain untrusted here too.
            transition(
                repo,
                attempt_id,
                "failed",
                "hard_budget_unsupported",
                "runner-hard-budget-unsupported",
            )
            return 78
        transition(repo, attempt_id, "review", key="runner-review")
        if job.get("completion_state") == "done":
            transition(repo, attempt_id, "done", key="runner-done")
        return 0
    except BaseException:
        cleanup_ok = False
        try:
            stop_process_tree(process, process_identity_value)
            cleanup_ok = True
        except (OSError, subprocess.SubprocessError, ae.OpsError):
            pass
        try:
            capture.wait()
        except (OSError, ae.OpsError):
            pass
        try:
            with ae.file_lock(lock_target):
                job = load_job(repo, attempt_id)
                job.update({
                    "runtime_state": "interrupted" if cleanup_ok else "cleanup_failed",
                    "exit_code": process.returncode,
                    "pid": 0 if cleanup_ok else process.pid,
                    "pid_identity": "" if cleanup_ok else process_identity(process.pid),
                    "runner_pid": 0,
                    "runner_pid_identity": "",
                    "finished_at": ae.utc_now(),
                })
                save_job(repo, job)
        except (OSError, ValueError, ae.OpsError):
            pass
        try:
            _, attempts = ae.load_projection(repo)
            if attempts.get(attempt_id, {}).get("state") in ae.NONTERMINAL_STATES:
                transition(repo, attempt_id, "blocked", "supervisor_error", "runner-interrupted")
        except (OSError, ValueError, ae.OpsError):
            pass
        raise


def dispatch(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    limits: Dict[str, int] = {}
    for raw in args.provider_limit:
        if "=" not in raw:
            raise ae.OpsError("provider-limit must be PROVIDER=N")
        provider, value = raw.split("=", 1)
        provider = ae.safe_id(provider, "provider")
        try:
            limit = int(value)
        except ValueError as exc:
            raise ae.OpsError("provider-limit must end in an integer") from exc
        if limit < 1:
            raise ae.OpsError("provider-limit must be positive")
        limits[provider] = limit
    launched: List[str] = []
    with ae.file_lock(queue_lock_path(repo)):
        jobs = all_jobs(repo, strict=True)
        active = [
            job
            for job in jobs
            if job.get("runtime_state")
            in {"launching", "starting", "running", "cancel_requested", "cleanup_failed"}
        ]
        active_by_provider: Dict[str, int] = {}
        for job in active:
            provider = job.get("provider", "local")
            active_by_provider[provider] = active_by_provider.get(provider, 0) + 1
        reserved_tokens = sum(int(job.get("reserved_tokens") or 0) for job in active)
        reserved_cost = sum(int(job.get("reserved_cost_microusd") or 0) for job in active)
        queued = sorted(
            [job for job in jobs if job.get("runtime_state") == "queued"],
            key=lambda job: (
                int(job.get("queue_order") or 0),
                job.get("created_at", ""),
                job.get("attempt_id", ""),
            ),
        )
        for job in queued:
            if len(launched) >= args.max_jobs or len(active) + len(launched) >= args.max_running:
                break
            provider = job.get("provider", "local")
            if provider in limits and active_by_provider.get(provider, 0) >= limits[provider]:
                continue
            token_reserve = job.get("budget", {}).get("max_tokens")
            cost_reserve = job.get("budget", {}).get("max_cost_microusd")
            if args.max_total_tokens is not None:
                if token_reserve is None or reserved_tokens + int(token_reserve) > args.max_total_tokens:
                    continue
            if args.max_total_cost_microusd is not None:
                if cost_reserve is None or reserved_cost + int(cost_reserve) > args.max_total_cost_microusd:
                    continue
            job["runtime_state"] = "launching"
            job["launch_started_at"] = ae.utc_now()
            job["launch_nonce"] = uuid.uuid4().hex
            job["reserved_tokens"] = int(token_reserve or 0)
            job["reserved_cost_microusd"] = int(cost_reserve or 0)
            save_job(repo, job)
            launched.append(job["attempt_id"])
            active_by_provider[provider] = active_by_provider.get(provider, 0) + 1
            reserved_tokens += int(token_reserve or 0)
            reserved_cost += int(cost_reserve or 0)
    launched_ids: List[str] = []
    failures: List[str] = []
    for attempt_id in launched:
        cancelled_before_spawn = False
        with ae.file_lock(queue_lock_path(repo)):
            job = load_job(repo, attempt_id)
            if job.get("runtime_state") == "cancel_requested":
                _finish_job(job, "cancelled")
                save_job(repo, job)
                cancelled_before_spawn = True
        if cancelled_before_spawn:
            transition(
                repo,
                attempt_id,
                "cancelled",
                "cancelled_by_owner",
                "dispatch-cancelled-before-spawn",
            )
            continue
        try:
            launched_process = subprocess.Popen(
                [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "--repo",
                    str(repo),
                    "_run-one",
                    "--attempt",
                    attempt_id,
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
        except (OSError, ValueError):
            with ae.file_lock(queue_lock_path(repo)):
                job = load_job(repo, attempt_id)
                _finish_job(job, "interrupted")
                save_job(repo, job)
            transition(
                repo,
                attempt_id,
                "blocked",
                "runner_launch_failed",
                "dispatch-runner-launch-failed",
            )
            failures.append(attempt_id)
            continue
        with ae.file_lock(queue_lock_path(repo)):
            job = load_job(repo, attempt_id)
            if job.get("runtime_state") == "launching":
                job["runner_pid"] = launched_process.pid
                job["runner_pid_identity"] = process_identity(launched_process.pid)
                save_job(repo, job)
        launched_ids.append(attempt_id)
        print(attempt_id)
    args.launched_ids = launched_ids
    if failures:
        raise ae.OpsError("could not launch runner for: %s" % ", ".join(failures))
    return 0


def status(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    row = public_job(repo, load_job(repo, ae.safe_id(args.attempt, "attempt")))
    if args.json:
        ae.print_json(row)
    else:
        print("attempt: %s" % row["attempt_id"])
        print("lifecycle/runtime: %s/%s" % (row["state"], row["runtime_state"]))
        print("provider/profile: %s/%s" % (row["provider"], row["profile"]))
        print("exit: %s" % (row["exit_code"] if row["exit_code"] is not None else "-"))
    return 0


def wait_for(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    attempt_id = ae.safe_id(args.attempt, "attempt")
    deadline = time.monotonic() + args.timeout
    while True:
        row = public_job(repo, load_job(repo, attempt_id))
        # Review settles a wait only for a job whose submit contract ends
        # there. A job submitted with --completion-state done passes through
        # review on its way to done, and returning at review handed callers a
        # projection that had not landed yet.
        settled = row["state"] in ae.TERMINAL_STATES or (
            row["state"] == "review" and row.get("completion_state") != "done"
        )
        if settled:
            if args.json:
                ae.print_json(row)
            else:
                print("%s %s" % (attempt_id, row["state"]))
            return 0 if row["state"] in {"done", "review"} else 1
        if time.monotonic() >= deadline:
            raise ae.OpsError("wait timed out while attempt remained %s" % row["state"])
        time.sleep(args.interval)


def attach(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    attempt_id = ae.safe_id(args.attempt, "attempt")
    load_job(repo, attempt_id)
    path = log_path(repo, attempt_id)
    lines: "collections.deque[str]" = collections.deque(maxlen=args.tail)
    try:
        with path.open(encoding="utf-8", errors="replace") as handle:
            for line in handle:
                lines.append(line.rstrip("\r\n"))
    except FileNotFoundError:
        return 0
    for line in lines:
        print(line)
    return 0


def cancel(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    attempt_id = ae.safe_id(args.attempt, "attempt")
    signal_target: Optional[Tuple[int, str]] = None
    identity_failure = False
    with ae.file_lock(queue_lock_path(repo)):
        job = load_job(repo, attempt_id)
        state = job.get("runtime_state")
        if state == "queued":
            _finish_job(job, "cancelled")
            save_job(repo, job)
            transition(repo, attempt_id, "cancelled", "cancelled_by_owner", "owner-cancelled")
        elif state in {
            "launching",
            "starting",
            "running",
            "cancel_requested",
            "cleanup_failed",
        }:
            pid = int(job.get("pid") or 0)
            pid_identity = str(job.get("pid_identity") or "")
            pid_or_group_alive = bool(
                pid and (ae.pid_alive(pid) or process_group_alive(pid))
            )
            if pid_or_group_alive and not recorded_tree_matches(pid, pid_identity):
                # The numeric PID/group is live but no longer proves ownership.
                # Clear it and report failure without signalling an unrelated
                # process. A still-live runner retains its own Popen identity
                # and will independently converge when it observes this state.
                identity_failure = True
                job.update({
                    "runtime_state": "interrupted",
                    "exit_code": None,
                    "pid": 0,
                    "pid_identity": "",
                    "finished_at": ae.utc_now(),
                })
            else:
                job["runtime_state"] = "cancel_requested"
                if pid and recorded_tree_matches(pid, pid_identity):
                    signal_target = (pid, pid_identity)
            save_job(repo, job)
        elif state in {"finished", "cancelled", "abandoned", "interrupted"}:
            raise ae.OpsError("attempt is no longer cancellable: %s" % state)
        else:
            raise ae.OpsError("unknown runtime state: %s" % state)
    if identity_failure:
        transition(
            repo,
            attempt_id,
            "failed",
            "process_identity_mismatch",
            "process-identity-mismatch",
        )
    elif signal_target is not None:
        pid, pid_identity = signal_target
        if not terminate_process_group(pid, pid_identity) and (
            ae.pid_alive(pid) or process_group_alive(pid)
        ):
            late_identity_failure = False
            with ae.file_lock(queue_lock_path(repo)):
                job = load_job(repo, attempt_id)
                if (
                    job.get("runtime_state") == "cancel_requested"
                    and int(job.get("pid") or 0) == pid
                    and str(job.get("pid_identity") or "") == pid_identity
                ):
                    job.update({
                        "runtime_state": "interrupted",
                        "exit_code": None,
                        "pid": 0,
                        "pid_identity": "",
                        "finished_at": ae.utc_now(),
                    })
                    save_job(repo, job)
                    late_identity_failure = True
            if late_identity_failure:
                transition(
                    repo,
                    attempt_id,
                    "failed",
                    "process_identity_mismatch",
                    "process-identity-mismatch",
                )
    print(attempt_id)
    return 0


def resume(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    parent_id = ae.safe_id(args.attempt, "attempt")
    parent_job = load_job(repo, parent_id)
    _, attempts = ae.load_projection(repo)
    parent = attempts.get(parent_id)
    if parent is None or parent["state"] not in ae.TERMINAL_STATES | {"blocked"}:
        raise ae.OpsError("attempt is still live; attach or wait instead")
    spec = json.loads(ae.attempt_spec_path(repo, parent_id).read_text(encoding="utf-8"))
    if spec.get("base_sha") and ae.git_head(repo) != spec["base_sha"]:
        raise ae.OpsError("repository HEAD changed; the prior job cannot be resumed")
    attempt_id = ae.create_attempt(
        repo,
        provider=parent_job.get("provider", "local"),
        tool="agent-supervisor",
        task_id=parent_job.get("task_id", ""),
        run_id=spec.get("run_id", ""),
        parent_attempt_id=parent_id,
        budget=parent_job.get("budget", {}),
        refs={"command_sha256": parent_job["command_sha256"]},
    )
    new_spec_path = ae.attempt_spec_path(repo, attempt_id)
    new_spec = json.loads(new_spec_path.read_text(encoding="utf-8"))
    new_spec.update({
        "profile": parent_job.get("profile", "trusted-local"),
        "completion_state": parent_job.get("completion_state", "review"),
        "command_sha256": parent_job["command_sha256"],
    })
    ae.write_json_atomic(new_spec_path, new_spec)
    new_job = dict(parent_job)
    new_job.update({
        "attempt_id": attempt_id,
        "runtime_state": "queued",
        "queue_order": time.time_ns(),
        "created_at": ae.utc_now(),
        "updated_at": ae.utc_now(),
        "exit_code": None,
        "pid": 0,
        "pid_identity": "",
        "runner_pid": 0,
        "runner_pid_identity": "",
        "parent_attempt_id": parent_id,
    })
    for key in (
        "finished_at",
        "reserved_tokens",
        "reserved_cost_microusd",
        "launch_started_at",
        "launch_nonce",
    ):
        new_job.pop(key, None)
    save_new_supervisor_job(repo, new_job)
    print(attempt_id)
    return 0


def reconcile(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    interrupted: List[str] = []
    with ae.file_lock(queue_lock_path(repo)):
        runtime_jobs = all_jobs(repo, strict=True)
        runtime_ids = {str(job.get("attempt_id") or "") for job in runtime_jobs}
        _, attempts = ae.load_projection(repo)
        now = ae.dt.datetime.now(ae.dt.timezone.utc)
        for attempt_id in sorted(attempts):
            attempt = attempts[attempt_id]
            if (
                attempt.get("state") != "queued"
                or attempt.get("tool") != "agent-supervisor"
                or attempt_id in runtime_ids
            ):
                continue
            try:
                age_seconds = (now - ae.parse_ts(attempt.get("created_at", ""))).total_seconds()
            except ae.OpsError:
                continue
            if age_seconds < ORPHAN_JOB_GRACE_SECONDS:
                continue
            # A new submit/resume publishes under this queue lock. Keep the
            # path recheck for compatibility with an older caller that began
            # before this reconciliation contract was installed.
            if job_path(repo, attempt_id).exists():
                continue
            interrupted.append(attempt_id)
            if args.apply:
                try:
                    transition(
                        repo,
                        attempt_id,
                        "blocked",
                        "runtime_job_missing",
                        "reconcile-runtime-job-missing",
                    )
                except ae.OpsError:
                    # Another lifecycle writer may have converged it first.
                    # Only suppress the race when it is no longer orphaned.
                    _, current_attempts = ae.load_projection(repo)
                    if current_attempts.get(attempt_id, {}).get("state") == "queued":
                        raise

        for job in runtime_jobs:
            runtime_state = job.get("runtime_state")
            if runtime_state not in {
                "launching",
                "starting",
                "running",
                "cancel_requested",
                "cleanup_failed",
            }:
                continue
            runner_pid = int(job.get("runner_pid") or 0)
            child_pid = int(job.get("pid") or 0)
            # A dead runner adopted by a deferring subreaper (autopilot
            # receipt supervise) stays an identity-matching zombie until the
            # phase exits; only a runner that can still run defers reconcile.
            runner_alive = process_matches(
                runner_pid, str(job.get("runner_pid_identity") or "")
            ) and not pid_is_zombie(runner_pid)
            if runner_alive:
                continue
            if runtime_state == "launching" and not runner_pid:
                try:
                    launch_age = (now - ae.parse_ts(job.get("launch_started_at", ""))).total_seconds()
                except ae.OpsError:
                    launch_age = 999
                if launch_age < 10:
                    continue

            child_identity = str(job.get("pid_identity") or "")
            child_alive = process_group_alive(child_pid)
            identity_mismatch = bool(
                child_pid
                and (ae.pid_alive(child_pid) or child_alive)
                and not recorded_tree_matches(child_pid, child_identity)
            )
            interrupted.append(job["attempt_id"])
            if args.apply:
                if identity_mismatch:
                    _finish_job(job, "interrupted")
                    save_job(repo, job)
                    _, attempts = ae.load_projection(repo)
                    if attempts.get(job["attempt_id"], {}).get("state") in ae.NONTERMINAL_STATES:
                        transition(
                            repo,
                            job["attempt_id"],
                            "failed",
                            "process_identity_mismatch",
                            "process-identity-mismatch",
                        )
                    continue
                if child_alive and not stop_recorded_tree(child_pid, child_identity):
                    job["runtime_state"] = "cleanup_failed"
                    job["runner_pid"] = 0
                    job["runner_pid_identity"] = ""
                    save_job(repo, job)
                    continue
                cancelled = runtime_state == "cancel_requested"
                _finish_job(job, "cancelled" if cancelled else "interrupted")
                save_job(repo, job)
                _, attempts = ae.load_projection(repo)
                state = attempts.get(job["attempt_id"], {}).get("state")
                if state in ae.NONTERMINAL_STATES and state != "blocked":
                    if cancelled:
                        transition(
                            repo,
                            job["attempt_id"],
                            "cancelled",
                            "cancelled_by_owner",
                            "reconcile-runner-cancelled",
                        )
                    else:
                        transition(
                            repo,
                            job["attempt_id"],
                            "blocked",
                            "runner_lost",
                            "reconcile-runner-lost",
                        )
    if args.json:
        ae.print_json(interrupted)
    else:
        for attempt_id in interrupted:
            print(attempt_id)
        if not interrupted:
            print("no interrupted attempts")
    return 0


def gc_runtime(args: argparse.Namespace) -> int:
    """Prune expired terminal runtime records, never lifecycle evidence."""
    repo = ae.repo_root(args.repo)
    cutoff = ae.dt.datetime.now(ae.dt.timezone.utc) - ae.dt.timedelta(days=args.older_than_days)
    selected: List[Dict[str, Any]] = []
    with ae.file_lock(queue_lock_path(repo)):
        _, attempts = ae.load_projection(repo)
        for job in all_jobs(repo):
            attempt_id = job.get("attempt_id", "")
            lifecycle_state = attempts.get(attempt_id, {}).get("state", "unknown")
            runtime_state = job.get("runtime_state", "unknown")
            if lifecycle_state not in ae.TERMINAL_STATES:
                continue
            if runtime_state not in {"finished", "cancelled", "abandoned"}:
                continue
            if process_matches(
                int(job.get("pid") or 0), str(job.get("pid_identity") or "")
            ) or process_matches(
                int(job.get("runner_pid") or 0),
                str(job.get("runner_pid_identity") or ""),
            ):
                continue
            age_value = job.get("finished_at") or job.get("updated_at") or job.get("created_at")
            try:
                age_at = ae.parse_ts(age_value)
            except ae.OpsError:
                # A malformed timestamp must fail safe by retaining the record.
                continue
            if age_at > cutoff:
                continue
            selected.append({
                "attempt_id": attempt_id,
                "terminal": True,
                "lifecycle_state": lifecycle_state,
                "runtime_state": runtime_state,
                "updated_at": age_value,
                "applied": bool(args.apply),
            })
            if args.apply:
                for path in (log_path(repo, attempt_id), job_path(repo, attempt_id)):
                    try:
                        path.unlink()
                    except FileNotFoundError:
                        pass
    selected.sort(key=lambda row: row["attempt_id"])
    if args.json:
        ae.print_json(selected)
    else:
        prefix = "deleted" if args.apply else "would delete"
        for row in selected:
            print("%s: %s" % (prefix, row["attempt_id"]))
        if not selected:
            print("no expired terminal runtime records")
    return 0


def serve(args: argparse.Namespace) -> int:
    repo = ae.repo_root(args.repo)
    started = time.monotonic()
    launched = 0
    while launched < args.max_jobs and time.monotonic() - started < args.wall_seconds:
        ns = argparse.Namespace(
            repo=str(repo), max_running=args.max_running, max_jobs=min(args.max_jobs - launched, args.max_running),
            provider_limit=args.provider_limit, max_total_tokens=args.max_total_tokens,
            max_total_cost_microusd=args.max_total_cost_microusd,
        )
        dispatch(ns)
        launched += len(getattr(ns, "launched_ids", []))
        queued = any(
            job.get("runtime_state") == "queued" for job in all_jobs(repo, strict=True)
        )
        active = any(
            job.get("runtime_state")
            in {"launching", "starting", "running", "cancel_requested", "cleanup_failed"}
            for job in all_jobs(repo, strict=True)
        )
        if not queued and (not active or not args.until_idle):
            break
        time.sleep(args.interval)
    return 0


def add_common_dispatch(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--max-running", type=int, default=1)
    parser.add_argument("--max-jobs", type=int, default=1)
    parser.add_argument("--provider-limit", action="append", default=[])
    parser.add_argument("--max-total-tokens", type=int)
    parser.add_argument("--max-total-cost-microusd", type=int)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="agent-supervisor.sh", description="Bounded durable process supervisor; never lands, commits, or pushes.")
    parser.add_argument("--repo", default=".")
    subs = parser.add_subparsers(dest="command", required=True)

    submit_parser = subs.add_parser("submit")
    submit_parser.add_argument("--provider", default="local")
    submit_parser.add_argument("--profile", default="trusted-local", choices=("trusted-local", "isolated", "remote"))
    submit_parser.add_argument("--completion-state", default="review", choices=("review", "done"))
    submit_parser.add_argument("--cwd", default="")
    submit_parser.add_argument("--task-id", default="")
    submit_parser.add_argument("--run-id", default="")
    submit_parser.add_argument("--max-wall-seconds", type=int, default=3600)
    submit_parser.add_argument("--max-log-bytes", type=int, default=DEFAULT_MAX_LOG_BYTES)
    submit_parser.add_argument("--max-tokens", type=int)
    submit_parser.add_argument("--max-cost-microusd", type=int)
    submit_parser.add_argument("command_argv", nargs=argparse.REMAINDER)
    submit_parser.set_defaults(func=submit)

    dispatch_parser = subs.add_parser("dispatch")
    add_common_dispatch(dispatch_parser)
    dispatch_parser.set_defaults(func=dispatch)

    status_parser = subs.add_parser("status")
    status_parser.add_argument("--attempt", required=True)
    status_parser.add_argument("--json", action="store_true")
    status_parser.set_defaults(func=status)

    wait_parser = subs.add_parser("wait")
    wait_parser.add_argument("--attempt", required=True)
    wait_parser.add_argument("--timeout", type=float, default=3600)
    wait_parser.add_argument("--interval", type=float, default=0.2)
    wait_parser.add_argument("--json", action="store_true")
    wait_parser.set_defaults(func=wait_for)

    attach_parser = subs.add_parser("attach")
    attach_parser.add_argument("--attempt", required=True)
    attach_parser.add_argument("--tail", type=int, default=80)
    attach_parser.set_defaults(func=attach)

    cancel_parser = subs.add_parser("cancel")
    cancel_parser.add_argument("--attempt", required=True)
    cancel_parser.set_defaults(func=cancel)

    resume_parser = subs.add_parser("resume")
    resume_parser.add_argument("--attempt", required=True)
    resume_parser.set_defaults(func=resume)

    reconcile_parser = subs.add_parser("reconcile")
    reconcile_parser.add_argument("--apply", action="store_true")
    reconcile_parser.add_argument("--json", action="store_true")
    reconcile_parser.set_defaults(func=reconcile)

    gc_parser = subs.add_parser("gc", help="prune expired terminal runtime records; dry-run by default")
    gc_parser.add_argument("--older-than-days", type=int, default=30)
    gc_parser.add_argument("--apply", action="store_true")
    gc_parser.add_argument("--json", action="store_true")
    gc_parser.set_defaults(func=gc_runtime)

    serve_parser = subs.add_parser("serve")
    add_common_dispatch(serve_parser)
    serve_parser.add_argument("--wall-seconds", type=int, default=300)
    serve_parser.add_argument("--until-idle", action="store_true")
    serve_parser.add_argument("--interval", type=float, default=0.5)
    serve_parser.set_defaults(func=serve)

    internal = subs.add_parser("_run-one")
    internal.add_argument("--attempt", required=True)
    internal.set_defaults(func=lambda args: runner(ae.repo_root(args.repo), ae.safe_id(args.attempt, "attempt")))
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(list(sys.argv[1:] if argv is None else argv))
    for name in ("max_running", "max_jobs", "wall_seconds"):
        if hasattr(args, name) and getattr(args, name) < 1:
            raise ae.OpsError("%s must be positive" % name.replace("_", "-"))
    if hasattr(args, "tail") and args.tail < 1:
        raise ae.OpsError("tail must be positive")
    if hasattr(args, "older_than_days") and not 0 <= args.older_than_days <= 36500:
        raise ae.OpsError("older-than-days must be between 0 and 36500")
    return int(args.func(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ae.OpsError as exc:
        print("error: %s" % exc, file=sys.stderr)
        raise SystemExit(2)
