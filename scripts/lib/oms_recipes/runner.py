"""Explicit, bounded host execution with durable attempt and output receipts."""

from __future__ import annotations

import contextlib
import hashlib
import os
from pathlib import Path
import selectors
import signal
import socket
import subprocess
import time
import uuid

from oms_runtime.common import (
    ABSOLUTE_PATH_RE, MAX_STATE_BYTES, CoreError, SECRET_VALUE_RE, atomic_write_json, bounded_line, file_lock, pretty_json, read_bytes,
    read_json, sha256_bytes, utc_now,
)
from .common import MAX_ATTEMPTS, MAX_RECORDS, history, records, state_path, state_root, under
from .definitions import _repo_path


def _output(repo: Path, requirement: dict) -> dict | None:
    path = _repo_path(repo, requirement["path"])
    if not path.exists():
        return None
    data = read_bytes(path, requirement.get("max_bytes", 16 * 1024 * 1024))
    return {"path": requirement["path"], "bytes": len(data), "sha256": sha256_bytes(data), "mtime_ns": path.stat().st_mtime_ns}


def _before(repo: Path, outputs: list[dict]) -> dict:
    return {row["path"]: _output(repo, row) for row in outputs}


def _verify_outputs(repo: Path, outputs: list[dict], before: dict) -> list[dict]:
    result = []
    for requirement in outputs:
        row = _output(repo, requirement)
        if row is None:
            raise CoreError("required output is missing: " + requirement["path"])
        if row["bytes"] < requirement.get("min_bytes", 0):
            raise CoreError("required output is too small: " + requirement["path"])
        if requirement.get("sha256", row["sha256"]) != row["sha256"]:
            raise CoreError("required output digest does not match: " + requirement["path"])
        if requirement.get("fresh", True) and before.get(row["path"]) == row:
            raise CoreError("required output was not refreshed: " + requirement["path"])
        result.append({key: row[key] for key in ("path", "bytes", "sha256")})
    return result


def _verify_completed(repo: Path, run: dict, plan: dict) -> None:
    if set(run.get("steps", {})) != {step["id"] for step in plan["steps"]}:
        raise CoreError("run steps do not match the current definition")
    for step in plan["steps"]:
        record = run["steps"][step["id"]]
        if record.get("status") not in {"pending", "running", "succeeded", "failed", "interrupted"}:
            raise CoreError("invalid stored step status")
        if not isinstance(record.get("attempts"), list) or len(record["attempts"]) > MAX_ATTEMPTS:
            raise CoreError("invalid or exhausted stored attempts")
        if record["status"] != "succeeded":
            continue
        if not record["attempts"] or record["attempts"][-1].get("status") != "succeeded":
            raise CoreError("completed step is missing a successful receipt")
        evidence = record["attempts"][-1].get("outputs", [])
        if {item.get("path") for item in evidence} != {item["path"] for item in step["outputs"]}:
            raise CoreError("completed step output evidence is incomplete")
        for requirement in step["outputs"]:
            actual = _output(repo, requirement)
            old = next(item for item in evidence if item["path"] == requirement["path"])
            if actual is None or any(actual[key] != old.get(key) for key in ("sha256", "bytes")):
                raise CoreError("completed output changed; start a new run: " + requirement["path"])


def _stop(process: subprocess.Popen) -> None:
    # Kill the owned process group even if its leader has already exited.
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(process.pid, signal.SIGTERM)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=2)
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(process.pid, signal.SIGKILL)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=2)


def _launch(repo: Path, command: dict, **streams) -> subprocess.Popen:
    cwd = _repo_path(repo, command["cwd"], dot=True)
    # Recheck executable/dependency bytes at the process boundary. Host execution
    # cannot prevent a hostile concurrent writer; it never claims isolation.
    if sha256_bytes(read_bytes(Path(command["executable"]).resolve(), 64 * 1024 * 1024)) != command["executable_digest"]:
        raise CoreError("declared executable changed after planning")
    for dependency in command["dependencies"]:
        if sha256_bytes(read_bytes(_repo_path(repo, dependency["path"]), 16 * 1024 * 1024)) != dependency["sha256"]:
            raise CoreError("declared command dependency changed after planning")
    return subprocess.Popen([command["executable"], *command["argv"][1:]], cwd=cwd,
                            env=command["environment"], stdin=subprocess.DEVNULL,
                            start_new_session=True, **streams)


def _execute(repo: Path, command: dict, companions: list[subprocess.Popen], on_start) -> dict:
    started = time.monotonic()
    process = None
    count = 0
    hashed = hashlib.sha256()
    sensitive = False
    scan_tail = b""
    reason = "exit"
    code = 127
    try:
        process = _launch(repo, command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        on_start(process.pid)
        assert process.stdout is not None
        os.set_blocking(process.stdout.fileno(), False)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            eof = False
            while True:
                if time.monotonic() - started >= command["timeout_seconds"]:
                    reason, code = "timeout", 124
                    break
                if any(companion.poll() is not None for companion in companions):
                    reason, code = "companion-exited", 125
                    break
                events = selector.select(0.05)
                for key, _ in events:
                    try:
                        chunk = os.read(key.fd, 65536)
                    except BlockingIOError:
                        continue
                    if not chunk:
                        eof = True
                        selector.unregister(key.fileobj)
                        break
                    hashed.update(chunk)
                    count += len(chunk)
                    scan = scan_tail + chunk
                    sensitive = sensitive or bool(SECRET_VALUE_RE.search(scan.decode("utf-8", "replace")))
                    scan_tail = scan[-512:]
                    if count > command["max_output_bytes"]:
                        reason, code = "output-limit", 125
                        break
                if reason != "exit":
                    break
                returncode = process.poll()
                if returncode is not None and (eof or not events):
                    code = returncode if returncode >= 0 else 128 - returncode
                    break
    except OSError:
        reason, code = "launch-failed", 127
    finally:
        if process is not None:
            _stop(process)
            if process.stdout:
                process.stdout.close()
    return {"exit": code, "reason": reason, "duration_seconds": round(time.monotonic() - started, 3),
            "console_bytes": count, "console_sha256": hashed.hexdigest(),
            "sensitive_console_detected": sensitive, "console_retained": False}


def _alive(pid) -> bool:
    if type(pid) is not int or pid <= 1:
        return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def load_run(repo: Path, run_id: str) -> dict:
    run = read_json(state_path(repo, "runs", run_id))
    if not isinstance(run, dict) or run.get("schema") != 1 or run.get("id") != run_id:
        raise CoreError("run is missing or invalid")
    return run


def execute(repo: Path, plan: dict, *, run_id: str = "", resume: bool = False,
            retry_failed: bool = False, retry_uncertain: bool = False, request_id: str = "") -> tuple[dict, int]:
    run_id = run_id or "run-" + uuid.uuid4().hex
    state_root(repo, create=True)
    path = state_path(repo, "runs", run_id)
    with file_lock(path):
        if resume:
            run = load_run(repo, run_id)
            if run.get("plan_digest") != plan["plan_digest"]:
                raise CoreError("definition, inputs, environment or declared dependencies changed; start a new run")
            _verify_completed(repo, run, plan)
            if run.get("status") == "succeeded":
                # Even a completed run is inspected against present final outputs.
                for old in run.get("outputs", []):
                    actual = _output(repo, {"path": old["path"], "max_bytes": 64 * 1024 * 1024})
                    if actual is None or any(actual[key] != old[key] for key in ("sha256", "bytes")):
                        raise CoreError("completed final output changed; start a new run")
                return run, 0
            uncertain = run.get("status") in {"running", "interrupted"}
            for row in run["steps"].values():
                if row["status"] in {"running", "interrupted"}:
                    uncertain = True
                    if row["attempts"] and _alive(row["attempts"][-1].get("pid")):
                        raise CoreError("a prior step process still appears live; resume cannot replay it")
            for row in run.get("companion_attempts", []):
                if row.get("status") == "running" and _alive(row.get("pid")):
                    raise CoreError("a prior companion still appears live; resume cannot duplicate it")
            if uncertain and not retry_uncertain:
                raise CoreError("interrupted execution has uncertain side effects; --retry-uncertain is required")
            if run.get("status") == "failed" and not retry_failed:
                raise CoreError("failed execution requires explicit --retry-failed")
        else:
            if path.exists():
                raise CoreError("run id already exists")
            if len(records(repo, "runs")) >= MAX_RECORDS:
                raise CoreError("recipe run capacity reached")
            run = {"schema": 1, "id": run_id, "recipe_id": plan["id"], "kind": plan["kind"], "action": plan["action"],
                   "source": plan["source"], "definition_digest": plan["definition_digest"], "overlay_digest": plan["overlay_digest"],
                   "plan_digest": plan["plan_digest"], "inputs": plan["inputs"], "ports": plan["ports"],
                   "companions": [row["id"] for row in plan["companions"]], "created_at": utc_now(),
                   "request_id": request_id or None, "outputs_before": _before(repo, plan["outputs"]),
                   "steps": {row["id"]: {"status": "pending", "attempts": []} for row in plan["steps"]},
                   "companion_attempts": [], "execution": {"profile": "trusted-local", "sandbox": False,
                   "network": "host", "credential" "s": "ambient host files remain readable", "process_groups": True}}
        run.update(status="running", updated_at=utc_now())
        atomic_write_json(path, run)
        history(repo, "run-resumed" if resume else "run-started", id=run_id, recipe_id=plan["id"], plan_digest=plan["plan_digest"])
        processes = []
        companion_rows = []
        current = None
        exit_code = 0
        try:
            pending = [row for row in plan["steps"] if run["steps"][row["id"]]["status"] != "succeeded"]
            if pending:
                for companion in plan["companions"]:
                    if len(run["companion_attempts"]) >= MAX_ATTEMPTS:
                        raise CoreError("companion attempt limit reached")
                    row = {"id": companion["id"], "status": "running", "started_at": utc_now(),
                           "command_digest": companion["command"]["command_digest"]}
                    if companion["ready_port"] is not None:
                        try:
                            with socket.socket() as probe:
                                probe.bind(("127.0.0.1", companion["ready_port"]))
                        except OSError:
                            raise CoreError("companion port is already in use")
                    run["companion_attempts"].append(row)
                    companion_rows.append(row)
                    atomic_write_json(path, run)
                    process = _launch(repo, companion["command"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    processes.append(process)
                    row["pid"] = process.pid
                    atomic_write_json(path, run)
                    if companion["ready_port"] is not None:
                        deadline = time.monotonic() + companion["startup_seconds"]
                        while True:
                            if process.poll() is not None:
                                raise CoreError("companion exited before readiness")
                            try:
                                with socket.create_connection(("127.0.0.1", companion["ready_port"]), timeout=0.1):
                                    break
                            except OSError:
                                if time.monotonic() >= deadline:
                                    raise CoreError("companion readiness timed out")
                                time.sleep(0.05)
            for step in pending:
                record = run["steps"][step["id"]]
                if len(record["attempts"]) >= MAX_ATTEMPTS:
                    raise CoreError("step attempt limit reached")
                before = _before(repo, step["outputs"])
                current = {"attempt": len(record["attempts"]) + 1, "status": "running", "started_at": utc_now(),
                           "command_digest": step["command"]["command_digest"], "environment_digest": step["command"]["environment_digest"],
                           "executable_digest": step["command"]["executable_digest"],
                           "executable_path_digest": step["command"]["executable_path_digest"], "dependencies": step["command"]["dependencies"]}
                if len(pretty_json(run)) + len(pretty_json(current)) > MAX_STATE_BYTES - 1024 * 1024:
                    current = None
                    raise CoreError("run receipt capacity reached; start a new run")
                record["attempts"].append(current)
                record["status"] = "running"
                atomic_write_json(path, run)

                def on_start(pid):
                    current["pid"] = pid
                    atomic_write_json(path, run)

                receipt = _execute(repo, step["command"], processes, on_start)
                current.update(receipt, finished_at=utc_now())
                if receipt["exit"] != 0:
                    current["status"] = record["status"] = "failed"
                    exit_code = receipt["exit"]
                    run["failure"] = {"step": step["id"], "reason": receipt["reason"]}
                    break
                current["outputs"] = _verify_outputs(repo, step["outputs"], before)
                current["status"] = record["status"] = "succeeded"
                atomic_write_json(path, run)
                history(repo, "step-succeeded", id=run_id, step=step["id"], attempt=current["attempt"])
                current = None
            if exit_code == 0:
                _verify_completed(repo, run, plan)
                run["outputs"] = _verify_outputs(repo, plan["outputs"], run["outputs_before"])
                run.pop("failure", None)
                run["status"] = "succeeded"
            else:
                run["status"] = "failed"
        except (KeyboardInterrupt, InterruptedError):
            run["status"] = "interrupted"
            if current is not None:
                current.update(status="interrupted", finished_at=utc_now())
            for row in run["steps"].values():
                if row["status"] == "running":
                    row["status"] = "interrupted"
            exit_code = 130
        except (CoreError, OSError) as exc:
            run["status"] = "failed"
            # Only our own bounded validation messages enter shared receipts.
            message = str(exc) if isinstance(exc, CoreError) else "local process or filesystem operation failed"
            run["failure"] = {"reason": ABSOLUTE_PATH_RE.sub("[path omitted]", bounded_line(message, 500))}
            if current is not None:
                current.update(status="failed", finished_at=utc_now())
            for row in run["steps"].values():
                if row["status"] == "running":
                    row["status"] = "failed"
            exit_code = 2
        finally:
            for process in reversed(processes):
                _stop(process)
            for row in companion_rows:
                row.update(status="stopped", finished_at=utc_now())
            run["updated_at"] = utc_now()
            atomic_write_json(path, run)
            history(repo, "run-finished", id=run_id, status=run["status"], exit=exit_code)
        return run, exit_code
