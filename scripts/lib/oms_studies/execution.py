"""Explicit argv execution with byte-bound tools and bounded diagnostics.

Commands are trusted local programs, not an OS sandbox. Inputs and declared
tool files are frozen; imported modules, services and environment are not.
"""
from __future__ import annotations

import contextlib
import os
import shutil
import signal
import subprocess
import threading
import time
from pathlib import Path
from typing import Any

from oms_runtime.common import CoreError, atomic_write_bytes, canonical_json, sha256_bytes, sha256_file
from .common import blob, local, obj, relative, snapshot, writable

MAX_LOG = 16384
TOKENS = {"{input}", "{output}", "{prediction}", "{case_id}", "{variant}", "{repeat}", "{params}"}


def freeze(repo: Path, raw: Any) -> dict:
    writable()
    spec = obj(raw, "command")
    unknown = set(spec) - {"argv", "files", "timeout_seconds"}
    if unknown:
        raise CoreError("unknown command field: %s" % sorted(unknown)[0])
    argv = spec.get("argv")
    if (not isinstance(argv, list) or not 1 <= len(argv) <= 128 or
            any(not isinstance(v, str) or not v or len(v) > 8192 or "\0" in v for v in argv)):
        raise CoreError("command argv must have 1-128 nonempty string tokens")
    if any(token.startswith("{file:") and not token.endswith("}") for token in argv):
        raise CoreError("file placeholders must occupy an entire argv token")
    files = spec.get("files", [])
    if not isinstance(files, list) or len(files) > 100 or len(set(map(str, files))) != len(files):
        raise CoreError("command files must be a unique list of at most 100 paths")
    frozen = {relative(path): snapshot(repo, path) for path in files}
    for token in argv:
        if token.startswith("{file:") and token.endswith("}"):
            if token[6:-1] not in frozen:
                raise CoreError("command file placeholder is not declared: %s" % token)
    if argv[0].startswith("{"):
        raise CoreError("argv[0] must name a real executable; use an interpreter for frozen scripts")
    executable = (str(Path(argv[0])) if Path(argv[0]).is_absolute() else
                  shutil.which(argv[0]) if "/" not in argv[0] else str(local(repo, argv[0])))
    if not executable or not Path(executable).is_file() or not os.access(executable, os.X_OK):
        raise CoreError("command executable is unavailable: %s" % argv[0])
    # Preserve the invocation path: resolving a venv's python symlink would
    # bypass that environment even though the underlying executable matches.
    executable = str(Path(executable).absolute())
    timeout = spec.get("timeout_seconds", 60)
    if type(timeout) is not int or not 1 <= timeout <= 3600:
        raise CoreError("timeout_seconds must be an integer from 1 to 3600")
    body = {"argv": list(argv), "files": frozen, "timeout_seconds": timeout,
            "executable": executable, "executable_sha256": sha256_file(Path(executable))}
    # The executable pathname is local routing, not a reproducibility identity.
    identity = {k: v for k, v in body.items() if k != "executable"}
    body["sha256"] = sha256_bytes(canonical_json(identity))
    return body


def verify(repo: Path, command: dict) -> None:
    identity = {k: v for k, v in command.items() if k not in ("executable", "sha256")}
    if sha256_bytes(canonical_json(identity)) != command.get("sha256"):
        raise CoreError("frozen command manifest hash mismatch")
    exe = Path(command["executable"])
    if not exe.is_file() or sha256_file(exe) != command["executable_sha256"]:
        raise CoreError("command executable bytes changed since capture")
    for desc in command["files"].values():
        blob(repo, desc)


def materialize(repo: Path, command: dict, cwd: Path, mapping: dict) -> list[str]:
    verify(repo, command)
    paths = {}
    for name, desc in command["files"].items():
        target = cwd / "tools" / relative(name)
        atomic_write_bytes(target, blob(repo, desc), mode=0o400)
        paths["{file:" + name + "}"] = str(target)
    values = dict(mapping, **paths)
    argv = [command["executable"]]
    for token in command["argv"][1:]:
        if token in TOKENS or token.startswith("{file:"):
            if token not in values:
                raise CoreError("unavailable command placeholder: %s" % token)
            argv.append(str(values[token]))
        else:
            argv.append(token)
    return argv


def run(argv: list[str], cwd: Path, timeout: int, *, env: dict | None = None) -> dict:
    """Drain both pipes while keeping only a bounded diagnostic prefix."""
    writable()
    started = time.monotonic()
    kept = {"stdout": bytearray(), "stderr": bytearray()}
    total = {"stdout": 0, "stderr": 0}

    def drain(stream, name):
        try:
            while True:
                chunk = stream.read(8192)
                if not chunk:
                    break
                total[name] += len(chunk)
                kept[name].extend(chunk[:max(0, MAX_LOG - len(kept[name]))])
        except (OSError, ValueError):
            pass

    try:
        process = subprocess.Popen(argv, cwd=str(cwd), stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=(os.name == "posix"), env=env)
    except OSError as exc:
        return {"status": "failed", "exit_code": None, "error": str(exc), "elapsed_seconds": 0}
    threads = [threading.Thread(target=drain, args=(getattr(process, name), name), daemon=True)
               for name in kept]
    for thread in threads:
        thread.start()
    status, error = "complete", ""
    try:
        process.wait(timeout=timeout)
        if process.returncode:
            status, error = "failed", "command exited %d" % process.returncode
    except subprocess.TimeoutExpired:
        status, error = "failed", "command exceeded timeout"
    except KeyboardInterrupt:
        status, error = "interrupted", "command interrupted"
    finally:
        # Kill remaining descendants too: a detached child can otherwise keep
        # inherited pipes open after the direct command has already exited.
        if os.name == "posix":
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGTERM)
        elif process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            if os.name == "posix":
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
            process.wait(timeout=2)
        for thread in threads:
            thread.join(timeout=2)
        if os.name == "posix" and any(thread.is_alive() for thread in threads):
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            for thread in threads:
                thread.join(timeout=2)
        for name in kept:
            stream = getattr(process, name)
            if stream and not any(thread.is_alive() for thread in threads):
                stream.close()
    return {"status": status, "exit_code": process.returncode, "error": error,
            "elapsed_seconds": round(time.monotonic() - started, 6),
            "stdout": bytes(kept["stdout"]).decode("utf-8", "replace"),
            "stderr": bytes(kept["stderr"]).decode("utf-8", "replace"),
            "log_truncated": any(total[k] > MAX_LOG for k in total)}


def execute(repo: Path, command: dict, cwd: Path, mapping: dict) -> dict:
    result = run(materialize(repo, command, cwd, mapping), cwd, command["timeout_seconds"])
    try:
        verify(repo, command)
        for name, desc in command["files"].items():
            if sha256_file(cwd / "tools" / name) != desc["sha256"]:
                raise CoreError("command modified a declared tool file")
    except (CoreError, OSError) as exc:
        if result["status"] != "interrupted":
            result.update(status="failed", error=str(exc))
    return result
