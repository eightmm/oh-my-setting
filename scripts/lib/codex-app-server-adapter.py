#!/usr/bin/env python3
"""Bounded read-only adapter from OMS provider calls to Codex app-server.

The adapter is deliberately narrower than the app-server protocol: one
ephemeral thread, one read-only turn, text input and text output. Server-side
approval/input requests are authority escalation and fail the call; they are
never auto-approved. The normal `codex exec` transport remains the default.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import queue
import stat
import subprocess
import sys
import threading
import time
from pathlib import Path

MAX_PROMPT_BYTES = 256 * 1024
MAX_LINE_BYTES = 1024 * 1024
MAX_OUTPUT_BYTES = 60_000
MAX_STDERR_BYTES = 16_000


class AdapterError(RuntimeError):
    pass


def strict_json(text: str) -> object:
    def pairs(values):
        result = {}
        for key, value in values:
            if key in result:
                raise AdapterError("duplicate JSON key from app-server")
            result[key] = value
        return result

    def constant(value):
        raise AdapterError("non-finite JSON value from app-server: %s" % value)

    try:
        value = json.loads(text, object_pairs_hook=pairs, parse_constant=constant)
    except (ValueError, RecursionError) as exc:
        raise AdapterError("invalid JSON from app-server") from exc

    def finite(node):
        if isinstance(node, float) and not math.isfinite(node):
            raise AdapterError("non-finite JSON value from app-server")
        if isinstance(node, list):
            for item in node:
                finite(item)
        elif isinstance(node, dict):
            for item in node.values():
                finite(item)

    finite(value)
    return value


def regular_bytes(path: Path, limit: int) -> bytes:
    try:
        before = path.lstat()
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_nlink != 1
            or before.st_size > limit
        ):
            raise AdapterError("prompt must be a bounded single-link regular file")
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = os.open(str(path), flags)
        try:
            opened = os.fstat(fd)
            if (
                not stat.S_ISREG(opened.st_mode)
                or opened.st_nlink != 1
                or (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            ):
                raise AdapterError("prompt changed while it was opened")
            data = b""
            while len(data) <= limit:
                chunk = os.read(fd, min(65536, limit + 1 - len(data)))
                if not chunk:
                    break
                data += chunk
        finally:
            os.close(fd)
    except OSError as exc:
        raise AdapterError("could not read prompt: %s" % exc) from exc
    if len(data) > limit:
        raise AdapterError("prompt exceeds %d-byte limit" % limit)
    return data


def command() -> list[str]:
    override = os.environ.get("OMS_CODEX_APP_SERVER_COMMAND", "")
    if not override:
        return ["codex", "app-server"]
    path = Path(override)
    try:
        info = path.lstat()
    except OSError as exc:
        raise AdapterError("app-server override is unreadable: %s" % exc) from exc
    if not path.is_absolute() or not stat.S_ISREG(info.st_mode) or not os.access(path, os.X_OK):
        raise AdapterError("app-server override must be an absolute executable regular file")
    return [str(path)]


class JsonLines:
    def __init__(self, process: subprocess.Popen[str], timeout: float):
        self.process = process
        self.deadline = time.monotonic() + timeout
        self.rows: queue.Queue[object] = queue.Queue()
        self.stderr = bytearray()
        self.stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        try:
            for line in self.process.stdout:
                if len(line.encode("utf-8", errors="replace")) > MAX_LINE_BYTES:
                    self.rows.put(AdapterError("app-server response line exceeds byte limit"))
                    return
                try:
                    row = strict_json(line)
                except AdapterError as exc:
                    self.rows.put(exc)
                    return
                self.rows.put(row)
        finally:
            self.rows.put(None)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        while True:
            chunk = self.process.stderr.buffer.read(4096)
            if not chunk:
                return
            room = MAX_STDERR_BYTES - len(self.stderr)
            if room > 0:
                self.stderr.extend(chunk[:room])

    def send(self, row: dict) -> None:
        assert self.process.stdin is not None
        try:
            self.process.stdin.write(json.dumps(row, separators=(",", ":")) + "\n")
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            raise AdapterError("app-server closed its input") from exc

    def receive(self) -> dict:
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise AdapterError("app-server turn timed out")
        try:
            row = self.rows.get(timeout=remaining)
        except queue.Empty as exc:
            raise AdapterError("app-server turn timed out") from exc
        if isinstance(row, Exception):
            raise row
        if row is None:
            detail = self.stderr.decode("utf-8", errors="replace").strip()
            raise AdapterError("app-server exited before turn completion%s" % (
                ": " + detail if detail else ""
            ))
        if not isinstance(row, dict):
            raise AdapterError("app-server message must be a JSON object")
        return row

    def response(self, request_id: int) -> dict:
        while True:
            row = self.receive()
            if "id" in row and "method" in row:
                raise AdapterError("app-server approval request was refused: %s" % row.get("method"))
            if row.get("id") != request_id:
                continue
            error = row.get("error")
            if error is not None:
                raise AdapterError("app-server request failed")
            result = row.get("result")
            if not isinstance(result, dict):
                raise AdapterError("app-server response has no result object")
            return result


def stop_process(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=2)


def run(args: argparse.Namespace) -> str:
    repo = Path(args.repo)
    try:
        repo = Path(os.path.realpath(repo))
    except (OSError, ValueError) as exc:
        raise AdapterError("invalid repository path: %s" % exc) from exc
    if not repo.is_dir():
        raise AdapterError("repository is not a directory")
    prompt_bytes = regular_bytes(Path(args.prompt_file), MAX_PROMPT_BYTES)
    try:
        prompt = prompt_bytes.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise AdapterError("prompt must be UTF-8 text") from exc
    if not prompt.strip():
        raise AdapterError("prompt is empty")

    try:
        process = subprocess.Popen(
            command(),
            cwd=str(repo),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
    except OSError as exc:
        raise AdapterError("could not start app-server: %s" % exc) from exc
    wire = JsonLines(process, args.timeout)
    try:
        wire.send({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"clientInfo": {"name": "oh-my-setting", "version": "1"},
                       "capabilities": {}},
        })
        wire.response(1)
        wire.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        thread_params = {
            "cwd": str(repo),
            "sandbox": "read-only",
            "approvalPolicy": "never",
            "ephemeral": True,
        }
        if args.model != "provider-default":
            thread_params["model"] = args.model
        wire.send({"jsonrpc": "2.0", "id": 2, "method": "thread/start", "params": thread_params})
        thread_result = wire.response(2)
        thread = thread_result.get("thread")
        thread_id = thread.get("id") if isinstance(thread, dict) else None
        if not isinstance(thread_id, str) or not thread_id:
            raise AdapterError("app-server returned no thread id")
        turn_params = {
            "threadId": thread_id,
            "cwd": str(repo),
            "input": [{"type": "text", "text": prompt}],
            "approvalPolicy": "never",
            "sandboxPolicy": {"type": "readOnly", "networkAccess": False},
        }
        if args.model != "provider-default":
            turn_params["model"] = args.model
        if args.effort:
            turn_params["effort"] = args.effort
        wire.send({"jsonrpc": "2.0", "id": 3, "method": "turn/start", "params": turn_params})
        wire.response(3)

        parts: list[str] = []
        size = 0
        while True:
            row = wire.receive()
            if "id" in row and "method" in row:
                raise AdapterError("app-server approval request was refused: %s" % row.get("method"))
            method = row.get("method")
            params = row.get("params")
            if method == "item/agentMessage/delta" and isinstance(params, dict):
                delta = params.get("delta")
                if not isinstance(delta, str):
                    raise AdapterError("app-server emitted a malformed agent message")
                size += len(delta.encode("utf-8"))
                if size > MAX_OUTPUT_BYTES:
                    raise AdapterError("app-server answer exceeds %d-byte limit" % MAX_OUTPUT_BYTES)
                parts.append(delta)
            elif method == "turn/completed":
                if not isinstance(params, dict):
                    raise AdapterError("app-server emitted malformed turn completion")
                turn = params.get("turn")
                status = turn.get("status") if isinstance(turn, dict) else None
                if status != "completed":
                    raise AdapterError("app-server turn did not complete successfully")
                break
            elif method and (
                "requestApproval" in method
                or "requestPermissions" in method
                or "requestUserInput" in method
            ):
                raise AdapterError("app-server approval request was refused: %s" % method)
        answer = "".join(parts)
        if not answer.strip():
            raise AdapterError("app-server completed without an agent message")
        return answer
    finally:
        stop_process(process)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--repo", required=True)
    result.add_argument("--prompt-file", required=True)
    result.add_argument("--model", default="provider-default")
    result.add_argument("--effort", default="")
    result.add_argument("--timeout", type=float, default=900.0)
    return result


def main() -> int:
    args = parser().parse_args()
    if not 1 <= args.timeout <= 3600:
        print("error: timeout must be between 1 and 3600 seconds", file=sys.stderr)
        return 2
    try:
        answer = run(args)
    except AdapterError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2
    print(answer, end="" if answer.endswith("\n") else "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
