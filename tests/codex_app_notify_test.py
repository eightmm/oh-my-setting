#!/usr/bin/env python3
"""Contract tests for the Codex app notification relay, against a fake app-server."""

import base64
import hashlib
import json
import os
import pathlib
import socket
import struct
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
import codex_app_notify  # noqa: E402
import hook_state  # noqa: E402


class FakeAppServer:
    """Unix-socket WebSocket JSON-RPC server that records every request."""

    def __init__(self, path, fail=()):
        self.calls, self.fail = [], set(fail)
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.bind(path)
        self.sock.listen(4)
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self.session, args=(conn,), daemon=True).start()

    def session(self, conn):
        head = b""
        while b"\r\n\r\n" not in head:
            head += conn.recv(4096)
        key = [l.split(b":", 1)[1].strip() for l in head.split(b"\r\n") if l.lower().startswith(b"sec-websocket-key")][0]
        accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest())
        conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                     b"Sec-WebSocket-Accept: " + accept + b"\r\n\r\n")
        reader = conn.makefile("rb")
        try:
            while True:
                first, second = reader.read(2)
                size = second & 0x7F
                if size == 126:
                    size = struct.unpack(">H", reader.read(2))[0]
                elif size == 127:
                    size = struct.unpack(">Q", reader.read(8))[0]
                mask = reader.read(4)
                data = bytes(b ^ mask[i % 4] for i, b in enumerate(reader.read(size)))
                msg = json.loads(data)
                self.calls.append(msg)
                if "id" not in msg:
                    continue
                if msg["method"] in self.fail:
                    self.reply(conn, {"id": msg["id"], "error": {"code": -32600, "message": "no rollout"}})
                    continue
                result = {"thread": {"id": "thread-new"}} if msg["method"] == "thread/start" else {}
                self.reply(conn, {"id": msg["id"], "result": result})
                if msg["method"] == "thread/shellCommand":
                    # The real server runs the command; capture what it would print.
                    command = msg["params"]["command"]
                    msg["printed"] = pathlib.Path(command.split(" ", 1)[1].strip("'")).read_text(encoding="utf-8")
                    self.reply(conn, {"method": "turn/completed", "params": {}})
        except (OSError, ValueError, TypeError):
            conn.close()

    @staticmethod
    def reply(conn, body):
        data = json.dumps(dict(body, jsonrpc="2.0")).encode()
        head = bytes([0x81, len(data)]) if len(data) < 126 else bytes([0x81, 126]) + struct.pack(">H", len(data))
        conn.sendall(head + data)

    def methods(self):
        return [c["method"] for c in self.calls]


@unittest.skipUnless(hasattr(socket, "AF_UNIX"), "the app-server control socket is a Unix socket")
class CodexAppNotifyTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        self.sock = str(self.dir / "app.sock")
        self.env = mock.patch.dict(os.environ, {"OMS_CODEX_NOTIFY": "1", "OMS_CODEX_NOTIFY_SOCK": self.sock,
                                                "XDG_STATE_HOME": str(self.dir / "state")})
        self.env.start()
        # A plain folder: the notification covers every project, adopted or not.
        self.repo = self.dir / "repo"
        self.repo.mkdir()

    def tearDown(self):
        self.env.stop()
        self.tmp.cleanup()

    def test_first_send_creates_the_one_chat_and_later_sends_reuse_it(self):
        server = FakeAppServer(self.sock)
        codex_app_notify.send(self.repo, "done $(touch pwned) `id`")
        self.assertEqual(server.methods(), ["initialize", "initialized", "thread/start",
                                            "thread/name/set", "thread/shellCommand"])
        self.assertEqual(codex_app_notify.thread_id(), "thread-new")
        self.assertEqual(server.calls[2]["params"]["config"], {"features": {"hooks": False}})
        # No model turn, and the message is file content, never shell text.
        shell = server.calls[4]
        self.assertNotIn("done", shell["params"]["command"])
        self.assertEqual(shell["printed"], "done $(touch pwned) `id`\n")
        self.assertEqual(list(codex_app_notify.state_path().parent.glob("*.msg")), [])
        server.calls.clear()
        codex_app_notify.send(self.repo, "again")
        self.assertEqual(server.methods(), ["initialize", "initialized", "thread/resume", "thread/shellCommand"])
        self.assertTrue(server.calls[2]["params"]["excludeTurns"])
        self.assertEqual(server.calls[3]["printed"], "again\n")

    def test_a_lost_chat_is_skipped_never_replaced(self):
        server = FakeAppServer(self.sock, fail={"thread/resume"})
        target = codex_app_notify.state_path()
        target.parent.mkdir(parents=True)
        target.write_text('{"thread_id": "thread-old"}', encoding="utf-8")
        with self.assertRaises(RuntimeError):
            codex_app_notify.send(self.repo, "done")
        self.assertNotIn("thread/start", server.methods())
        self.assertEqual(codex_app_notify.thread_id(), "thread-old")

    def test_stop_notifies_only_long_turns_when_the_socket_exists(self):
        payload = {"session_id": "s1"}
        hook_state.record_turn_start(payload)  # absent socket: nothing recorded
        self.assertFalse((self.dir / "state").exists())
        server = FakeAppServer(self.sock)
        hook_state.record_turn_start(payload)
        state = hook_state.turn_state_path(codex_app_notify, payload)
        self.assertTrue(state.exists())
        hook_state.write_json_atomic(state, {"prompt_at": time.time() - 10})
        hook_state.start_codex_notify(self.repo, payload, "short")
        hook_state.write_json_atomic(state, {"prompt_at": time.time() - 400})
        with mock.patch.dict(os.environ, {"OMS_CODEX_NOTIFY": "0"}):
            hook_state.start_codex_notify(self.repo, payload, "disabled")
        hook_state.start_codex_notify(self.repo, payload, "Fixed the relay.")
        deadline = time.time() + 20
        while "thread/shellCommand" not in server.methods() and time.time() < deadline:
            time.sleep(0.05)
        turns = [c for c in server.calls if c.get("method") == "thread/shellCommand"]
        self.assertEqual(len(turns), 1, server.methods())
        text = turns[0]["printed"]
        self.assertIn("repo (6분)", text)
        self.assertIn("Fixed the relay.", text)

    def test_a_turn_that_leaves_background_work_stays_silent(self):
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
        def row(kind, content, cwd=None):
            return json.dumps({"type": kind, "timestamp": stamp, "cwd": cwd,
                               "message": {"role": kind, "content": content}}) + "\n"
        transcript = self.dir / "session.jsonl"
        payload = {"session_id": "s2", "transcript_path": str(transcript)}
        rows = [
            row("user", "go", cwd=str(self.repo)),
            row("user", [{"type": "tool_result", "content": "Command running in background with ID: bwatch1. Output"}]),
            row("user", [{"type": "tool_result", "content": "Monitor started (task bmon2, expires in 25m)"}]),
            # Another session's launch notice quoted inside ordinary output.
            row("user", [{"type": "tool_result", "content": "log: Command running in background with ID: bquoted"}]),
        ]
        transcript.write_text("".join(rows), encoding="utf-8")
        self.assertEqual(hook_state.pending_background(payload), 2)
        rows.append(row("assistant", [{"type": "tool_use", "name": "TaskStop", "input": {"task_id": "bmon2"}}]))
        transcript.write_text("".join(rows), encoding="utf-8")
        self.assertEqual(hook_state.pending_background(payload), 1)

        server = FakeAppServer(self.sock)
        state = hook_state.turn_state_path(codex_app_notify, payload)
        drifted = self.dir / "memory"
        drifted.mkdir()
        with mock.patch.dict(os.environ, {"CLAUDE_PROJECT_DIR": ""}):
            for message in ("still training", "all done"):
                hook_state.write_json_atomic(state, {"prompt_at": time.time() - 400})
                hook_state.start_codex_notify(
                    hook_state.claude_session_project(payload) or drifted, payload, message)
                rows.append(row("user", "<task-notification> <task-id>bwatch1</task-id> "
                                        "<status>completed</status> </task-notification>"))
                transcript.write_text("".join(rows), encoding="utf-8")
            deadline = time.time() + 20
            while "thread/shellCommand" not in server.methods() and time.time() < deadline:
                time.sleep(0.05)
            time.sleep(0.2)
        printed = [c["printed"] for c in server.calls if c.get("method") == "thread/shellCommand"]
        self.assertEqual(len(printed), 1, server.methods())
        self.assertIn("🔔 Claude Code 작업 완료 · repo (6분)", printed[0])
        self.assertIn("all done", printed[0])

    def test_relay_ignores_the_notification_chat_rollout(self):
        target = codex_app_notify.state_path()
        target.parent.mkdir(parents=True)
        target.write_text('{"thread_id": "thread-notify"}', encoding="utf-8")
        day = self.dir / "codex" / "sessions" / "2026" / "01" / "01"
        day.mkdir(parents=True)
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
        (day / "rollout-x-thread-notify.jsonl").write_text(
            json.dumps({"type": "session_meta", "payload": {"cwd": str(self.repo), "source": "vscode"}}) + "\n"
            + json.dumps({"timestamp": stamp, "type": "event_msg",
                          "payload": {"type": "task_complete", "last_agent_message": "echo"}}) + "\n",
            encoding="utf-8")
        with mock.patch.dict(os.environ, {"OMS_CODEX_HOME": str(self.dir / "codex")}):
            self.assertEqual(hook_state.codex_rollout_card(self.repo.resolve(), 86400), {})


if __name__ == "__main__":
    unittest.main()
