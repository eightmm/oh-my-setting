#!/usr/bin/env python3
"""Surface a Claude Code turn completion as a Codex app notification.

The Codex desktop app raises its own notification when a turn completes in a
thread it has open. `thread/shellCommand` completes a turn without a model
call, so each notification is one `cat` of a message file in the machine's
single notification chat: no tokens, no quota. The command is fixed and the
message travels as file contents, never as shell text, because shellCommand
runs unsandboxed. The control socket speaks JSON-RPC over WebSocket
([experimental] upstream), so every failure here is silent.
"""

from __future__ import annotations

import base64
import contextlib
import json
import os
import shlex
import socket
import struct
import sys
from pathlib import Path

THREAD_NAME = "Claude 알림"


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
    def __init__(self, path: Path, timeout: float) -> None:
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(timeout)
        self.sock.connect(str(path))
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
            "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n" % key).encode())
        self.buf = b""
        while b"\r\n\r\n" not in self.buf:
            self._fill()
        head, self.buf = self.buf.split(b"\r\n\r\n", 1)
        if b" 101 " not in head.split(b"\r\n", 1)[0]:
            raise OSError("websocket upgrade refused")
        self.next_id = 0

    def _fill(self) -> None:
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
        size = len(data)
        if size < 126:
            head = bytes([0x80 | opcode, 0x80 | size])
        elif size < 65536:
            head = bytes([0x80 | opcode, 0x80 | 126]) + struct.pack(">H", size)
        else:
            head = bytes([0x80 | opcode, 0x80 | 127]) + struct.pack(">Q", size)
        mask = os.urandom(4)
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
            data = self._take(size)
            opcode = first & 0x0F
            if opcode == 9:
                self._send(10, data)
                continue
            if opcode == 8:
                raise OSError("app-server closed the connection")
            parts.append(data)
            if first & 0x80:
                return json.loads(b"".join(parts))

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
            if reply.get("id") == self.next_id and "method" not in reply:
                if "error" in reply:
                    raise RuntimeError("%s: %s" % (method, reply["error"]))
                return reply.get("result") or {}


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
            while client.message().get("method") != "turn/completed":
                pass
        finally:
            message.unlink()
    finally:
        client.sock.close()
    return 0


def write_atomic(target: Path, text: str) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_name(target.name + ".%d.tmp" % os.getpid())
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, target)


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
