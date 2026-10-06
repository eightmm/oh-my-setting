#!/usr/bin/env python3
"""Surface a Claude Code turn completion as a Codex app notification.

The Codex desktop app raises its own notification when a turn completes in a
thread it has open. `thread/shellCommand` completes a turn without a model
call, so each notification is one `cat` of a message file in the machine's
single notification chat: no tokens, no quota. The command is fixed and the
message travels as file contents, never as shell text, because shellCommand
runs unsandboxed. The control socket speaks JSON-RPC over WebSocket
([experimental] upstream). Legacy hooks retain silent best-effort sending;
panel delivery returns correlated receipts, including uncertain delivery.
"""

from __future__ import annotations

import base64
import contextlib
from collections import deque
import json
import os
import re
import shlex
import socket
import struct
import sys
import tempfile
import time
import unicodedata
from pathlib import Path

from work_journal import sanitize_multiline

THREAD_NAME = "Claude 알림"
MAX_FRAME = 1024 * 1024
MAX_EVENTS = 128


class RPCError(RuntimeError):
    """A server rejection is distinct from a lost submission response."""


def socket_path() -> Path:
    override = os.environ.get("OMS_CODEX_NOTIFY_SOCK")
    if override:
        return Path(override)
    home = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex")
    return home / "app-server-control" / "app-server-control.sock"


def state_path() -> Path:
    base = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state")
    return base / "oh-my-setting" / "codex-notify.json"


def thread_id() -> str:
    """The one notification chat for this machine; every repository shares it."""
    try:
        value = json.loads(state_path().read_text(encoding="utf-8")).get("thread_id")
    except (OSError, ValueError, AttributeError):
        return ""
    return value if isinstance(value, str) else ""


class Client:
    def __init__(self, path: Path, timeout: float, deadline: float = None) -> None:
        self.deadline = deadline if deadline is not None else time.monotonic() + timeout
        self.sock = socket.socket(socket.AF_UNIX)
        try:
            self._timeout()
            self.sock.connect(str(path))
            key = base64.b64encode(os.urandom(16)).decode()
            self._timeout()
            self.sock.sendall((
                "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n" % key).encode())
            self.buf = b""
            while b"\r\n\r\n" not in self.buf:
                self._fill()
                if len(self.buf) > 16384:
                    raise ValueError("websocket header too large")
            head, self.buf = self.buf.split(b"\r\n\r\n", 1)
            if b" 101 " not in head.split(b"\r\n", 1)[0]:
                raise OSError("websocket upgrade refused")
        except Exception:
            self.sock.close()
            raise
        self.next_id = 0
        self.events = deque()

    def _timeout(self) -> None:
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise socket.timeout("notification deadline")
        self.sock.settimeout(remaining)

    def _fill(self) -> None:
        self._timeout()
        chunk = self.sock.recv(65536)
        if not chunk:
            raise OSError("app-server closed the connection")
        self.buf += chunk

    def _take(self, size: int) -> bytes:
        while len(self.buf) < size:
            self._fill()
        out, self.buf = self.buf[:size], self.buf[size:]
        return out

    def _send(self, opcode: int, data: bytes) -> None:
        if len(data) > MAX_FRAME:
            raise ValueError("websocket message too large")
        size = len(data)
        if size < 126:
            head = bytes([0x80 | opcode, 0x80 | size])
        elif size < 65536:
            head = bytes([0x80 | opcode, 0x80 | 126]) + struct.pack(">H", size)
        else:
            head = bytes([0x80 | opcode, 0x80 | 127]) + struct.pack(">Q", size)
        mask = os.urandom(4)
        self._timeout()
        self.sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def message(self) -> dict:
        parts = []
        while True:
            first, second = self._take(2)
            size = second & 0x7F
            if size == 126:
                size = struct.unpack(">H", self._take(2))[0]
            elif size == 127:
                size = struct.unpack(">Q", self._take(8))[0]
            if size > MAX_FRAME or sum(map(len, parts)) + size > MAX_FRAME:
                raise ValueError("websocket frame too large")
            data = self._take(size)
            opcode = first & 0x0F
            if opcode == 9:
                self._send(10, data)
                continue
            if opcode == 8:
                raise OSError("app-server closed the connection")
            parts.append(data)
            if first & 0x80:
                result = json.loads(b"".join(parts))
                if not isinstance(result, dict):
                    raise ValueError("invalid JSON-RPC message")
                return result

    def notify(self, method: str, params: dict | None = None) -> None:
        body = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            body["params"] = params
        self._send(1, json.dumps(body).encode())

    def call(self, method: str, params: dict) -> dict:
        self.next_id += 1
        self._send(1, json.dumps({"jsonrpc": "2.0", "id": self.next_id,
                                  "method": method, "params": params}).encode())
        while True:
            reply = self.message()
            if "method" in reply:
                if len(self.events) >= MAX_EVENTS:
                    raise ValueError("too many notification events")
                self.events.append(reply)
                continue
            if reply.get("id") == self.next_id and "method" not in reply:
                if "error" in reply:
                    raise RPCError("%s: %s" % (method, reply["error"]))
                return reply.get("result") or {}

    def event(self) -> dict:
        return self.events.popleft() if self.events else self.message()


def send(repo: Path, text: str) -> int:
    path = socket_path()
    if not path.exists():
        return 0
    client = Client(path, float(os.environ.get("OMS_CODEX_NOTIFY_TIMEOUT", "30")))
    try:
        client.call("initialize", {"clientInfo": {"name": "oms-codex-notify", "version": "1"}})
        client.notify("initialized")
        # The notification chat must not re-enter OMS hooks.
        common = {"config": {"features": {"hooks": False}}}
        tid = thread_id()
        if tid:
            # A missing or deleted chat is skipped, never silently replaced.
            client.call("thread/resume", {"threadId": tid, "excludeTurns": True, **common})
        else:
            started = client.call("thread/start", {"cwd": str(repo), **common})
            tid = str(started["thread"]["id"])
            client.call("thread/name/set", {"threadId": tid, "name": THREAD_NAME})
            write_atomic(state_path(), json.dumps({"schema": 1, "thread_id": tid}) + "\n")
        message = state_path().with_name("codex-notify.%d.msg" % os.getpid())
        write_atomic(message, text.rstrip("\n") + "\n")
        try:
            client.call("thread/shellCommand", {"threadId": tid, "timeoutMs": 10000,
                                                "command": "cat " + shlex.quote(str(message))})
            while client.event().get("method") != "turn/completed":
                pass
        finally:
            message.unlink()
    finally:
        client.sock.close()
    return 0


def _safe_message(text: str, repo: Path) -> str:
    text = text[:8192]
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    text = "".join(c if c == "\n" or not unicodedata.category(c).startswith("C") else " " for c in text)
    text = re.sub(r"(?i)\b(password|token|api[_-]?key|secret)\s*[:=]\s*\S+", "[redacted]", text)
    text = re.sub(r"(?i)\bBearer\s+\S+", "[redacted]", text)
    text = re.sub(r"(?<![\w:])/(?:[^\s/]+/)*[^\s/]+", "[path]", text)
    text = re.sub(r"(?i)\b[A-Z]:\\(?:[^\s\\]+\\)*[^\s\\]+", "[path]", text)
    for path in (str(Path.home()), str(repo.resolve())):
        if path and path != "/":
            text = text.replace(path, "[path]")
    return sanitize_multiline(text, 2399).rstrip("\n") + "\n"


@contextlib.contextmanager
def _delivery_lock(deadline: float):
    lock = state_path().with_suffix(".lock")
    lock.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(str(lock), os.O_CREAT | os.O_RDWR, 0o600)
    with os.fdopen(fd, "r+b") as handle:
        if os.name == "nt":
            import msvcrt
            handle.write(b"\0")
            handle.flush()
            handle.seek(0)
            acquire = lambda: msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            release = lambda: msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl
            acquire = lambda: fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            release = lambda: fcntl.flock(handle, fcntl.LOCK_UN)
        while True:
            try:
                acquire()
                break
            except OSError:
                if time.monotonic() >= deadline:
                    raise socket.timeout("notification lock deadline")
                time.sleep(min(0.05, max(0, deadline - time.monotonic())))
        try:
            yield
        finally:
            release()


def _receipt(status: str, tid: str = "", turn: str = "", acknowledged: bool = False,
             completed: bool = False, persisted: bool = False, submitted: bool = False,
             delivery_unknown=None) -> dict:
    if delivery_unknown is None:
        delivery_unknown = submitted and status in ("timeout", "rpc_error", "persistence_failure")
    return {"schema": 1, "status": status, "reason": status if status != "persisted" else "",
            "thread_id": tid, "turn_id": turn, "acknowledged": acknowledged,
            "completed": completed, "persisted": persisted,
            "submitted": submitted, "delivery_unknown": delivery_unknown,
            "desktop_notification_observed": False}


def _event_ids(params: dict) -> tuple:
    turn = params.get("turn") or {}
    return params.get("threadId"), params.get("turnId") or (turn.get("id") if isinstance(turn, dict) else None)


def _is_archived(thread: dict) -> bool:
    return (thread.get("archived") is True or thread.get("isArchived") is True
            or thread.get("archivedAt") is not None or thread.get("status") == "archived")


def _command_matches(value, expected):
    if not isinstance(value, str):
        return False
    try:
        supplied, requested = shlex.split(value), shlex.split(expected)
        if supplied == requested:
            return True
        # App-server records the actual shell invocation around shellCommand.
        return (len(supplied) == 3 and Path(supplied[0]).name in ("bash", "sh", "zsh")
                and supplied[1] in ("-c", "-lc") and shlex.split(supplied[2]) == requested)
    except ValueError:
        return False


def _persisted(result: dict, tid: str, turn_id: str, item_id: str,
               command: str, expected: str) -> bool:
    thread = result.get("thread")
    if not isinstance(thread, dict) or thread.get("id") != tid or _is_archived(thread):
        return False
    turns = thread.get("turns")
    if not isinstance(turns, list) or len(turns) > MAX_EVENTS:
        return False
    for turn in turns:
        if not isinstance(turn, dict) or turn.get("id") != turn_id:
            continue
        items = turn.get("items")
        if not isinstance(items, list) or len(items) > MAX_EVENTS:
            return False
        return any(isinstance(item, dict) and item.get("id") == item_id
                   and item.get("type") == "commandExecution"
                   and _command_matches(item.get("command"), command) and item.get("exitCode") == 0
                   and item.get("aggregatedOutput") == expected for item in items)
    return False


def _read_persisted(client, tid, turn_id, item_id, command, expected):
    # A long receiver history must not require fetching other chats' output.
    try:
        page = client.call("thread/items/list", {"threadId": tid, "turnId": turn_id, "limit": 32})
    except RuntimeError as error:
        message = str(error).lower()
        if not any(word in message for word in ("unsupported", "method not found", "-32601")):
            raise
        return _persisted(client.call("thread/read", {"threadId": tid, "includeTurns": True}),
                          tid, turn_id, item_id, command, expected)
    entries = page.get("data")
    if not isinstance(entries, list) or len(entries) > 32:
        return False
    for entry in entries:
        if not isinstance(entry, dict) or entry.get("turnId") != turn_id:
            continue
        item = entry.get("item")
        if (isinstance(item, dict) and item.get("id") == item_id
                and item.get("type") == "commandExecution" and item.get("status") == "completed"
                and _command_matches(item.get("command"), command) and item.get("exitCode") == 0
                and item.get("aggregatedOutput") == expected):
            return True
    return False


def deliver(repo: Path, text: str, timeout: float = None) -> dict:
    """Send only to the saved receiver and prove this command's stored output."""
    if os.environ.get("OMS_CODEX_NOTIFY", "1") == "0":
        return _receipt("disabled")
    path = socket_path()
    tid = thread_id()
    if not tid or not path.exists():
        return _receipt("absent", tid)
    try:
        budget = float(timeout if timeout is not None else os.environ.get("OMS_CODEX_NOTIFY_TIMEOUT", "30"))
        if not 0 < budget <= 120:
            return _receipt("invalid_timeout", tid)
    except (TypeError, ValueError):
        return _receipt("invalid_timeout", tid)
    deadline = time.monotonic() + budget
    submitted = acknowledged = completed = False
    turn_id = ""
    try:
        with _delivery_lock(deadline):
            client = Client(path, budget, deadline)
            try:
                client.call("initialize", {"clientInfo": {"name": "oms-codex-notify", "version": "1"},
                                          "capabilities": {"experimentalApi": True}})
                client.notify("initialized")
                try:
                    before = client.call("thread/read", {"threadId": tid, "includeTurns": False})
                except RuntimeError as error:
                    if "no rollout" in str(error).lower() or "not found" in str(error).lower():
                        return _receipt("absent", tid)
                    raise
                thread = before.get("thread")
                if not isinstance(thread, dict) or thread.get("id") != tid:
                    return _receipt("absent", tid)
                if _is_archived(thread):
                    return _receipt("archived", tid)
                try:
                    resumed = client.call("thread/resume", {"threadId": tid, "excludeTurns": True,
                                                            "config": {"features": {"hooks": False}}})
                except RuntimeError as error:
                    if "archived" in str(error).lower():
                        return _receipt("archived", tid)
                    raise
                status = resumed.get("thread", {}).get("status", {})
                if isinstance(status, dict) and status.get("type") == "active":
                    return _receipt("busy", tid)
                message = state_path().with_name("codex-notify.%d.msg" % os.getpid())
                write_atomic(message, _safe_message(text, repo))
                try:
                    command = "cat " + shlex.quote(str(message))
                    submitted = True
                    ack = client.call("thread/shellCommand", {
                        "threadId": tid, "timeoutMs": min(10000, max(1, int((deadline - time.monotonic()) * 1000))),
                        "command": command})
                    acknowledged = True
                    if isinstance(ack, dict) and isinstance(ack.get("turnId"), str):
                        turn_id = ack["turnId"]
                    item_id = ""
                    item_ok = False
                    turn_done = False
                    seen_events = 0
                    while not (item_ok and turn_done):
                        seen_events += 1
                        if seen_events > MAX_EVENTS:
                            raise ValueError("too many notification events")
                        event = client.event()
                        params = event.get("params")
                        if not isinstance(params, dict):
                            continue
                        event_tid, event_turn = _event_ids(params)
                        if event_tid != tid or not isinstance(event_turn, str) or not event_turn:
                            continue
                        item = params.get("item")
                        method = event.get("method")
                        if method == "item/started" and isinstance(item, dict):
                            if item.get("type") == "commandExecution" and _command_matches(item.get("command"), command):
                                if not turn_id or turn_id == event_turn:
                                    turn_id, item_id = event_turn, item.get("id", "")
                        elif method == "item/completed" and isinstance(item, dict):
                            if not turn_id and _command_matches(item.get("command"), command):
                                turn_id, item_id = event_turn, item.get("id", "")
                            if event_turn == turn_id and item.get("id") == item_id and item_id:
                                if item.get("type") != "commandExecution" or item.get("exitCode") != 0:
                                    return _receipt("failed_command", tid, turn_id, acknowledged, submitted=True)
                                item_ok = True
                        elif method == "turn/completed" and event_turn == turn_id:
                            turn = params.get("turn")
                            if isinstance(turn, dict) and turn.get("status", "completed") != "completed":
                                return _receipt("failed_command", tid, turn_id, acknowledged, submitted=True)
                            turn_done = True
                    completed = True
                    if not _read_persisted(client, tid, turn_id, item_id, command,
                                           _safe_message(text, repo)):
                        return _receipt("persistence_failure", tid, turn_id, acknowledged, completed, submitted=True)
                    return _receipt("persisted", tid, turn_id, acknowledged, completed, True, submitted=True)
                finally:
                    message.unlink(missing_ok=True)
            finally:
                client.sock.close()
    except socket.timeout:
        return _receipt("timeout", tid, turn_id, acknowledged, completed, submitted=submitted)
    except RPCError:
        return _receipt("rpc_error", tid, turn_id, acknowledged, completed,
                        submitted=submitted, delivery_unknown=acknowledged)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError, AttributeError):
        return _receipt("rpc_error", tid, turn_id, acknowledged, completed, submitted=submitted)


def write_atomic(target: Path, text: str) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=str(target.parent),
                                         prefix=target.name + ".", suffix=".tmp", delete=False) as handle:
            tmp = Path(handle.name)
            handle.write(text)
        os.replace(tmp, target)
    finally:
        if tmp is not None:
            tmp.unlink(missing_ok=True)


def main() -> int:
    if len(sys.argv) != 4 or sys.argv[1] != "send":
        print("usage: codex_app_notify.py send REPO TEXT", file=sys.stderr)
        return 2
    repo = Path(sys.argv[2])
    lock = state_path().with_suffix(".lock")
    # One sender at a time, so two Stops never race to create two chats.
    with contextlib.suppress(Exception):
        lock.parent.mkdir(parents=True, exist_ok=True)
        with lock.open("w") as handle:
            with contextlib.suppress(ImportError, OSError):
                import fcntl
                fcntl.flock(handle, fcntl.LOCK_EX)
            return send(repo, sys.argv[3])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
