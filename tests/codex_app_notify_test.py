#!/usr/bin/env python3
"""App delivery contracts against local fake Codex and Claude transports."""

import base64
import hashlib
import json
import os
import pathlib
import shlex
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
import claude_app_notify  # noqa: E402


class FakeAppServer:
    """Unix-socket WebSocket JSON-RPC server that records every request."""

    def __init__(self, path, fail=(), delivery=False, archived=False,
                 early=False, wrong=False, exit_code=0, persisted=True, silent=False,
                 busy=False, pagination=True, wrapped=False, disconnect_before_ack=False, malformed=False):
        self.calls, self.fail = [], set(fail)
        self.delivery, self.archived, self.early = delivery, archived, early
        self.wrong, self.exit_code, self.persisted, self.silent = wrong, exit_code, persisted, silent
        self.turns = []
        self.loaded, self.busy, self.pagination = False, busy, pagination
        self.wrapped = wrapped
        self.disconnect_before_ack = disconnect_before_ack
        self.malformed = malformed
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
                method = msg["method"]
                result = {"thread": {"id": "thread-new"}} if method == "thread/start" else {}
                if method == "thread/resume" and self.delivery:
                    self.loaded = True
                    result = {"thread": {"id": "thread-old", "status": {"type": "active" if self.busy else "idle"}}}
                    if self.malformed:
                        result = {"thread": None}
                if method == "thread/items/list" and self.delivery:
                    if not self.pagination:
                        self.reply(conn, {"id": msg["id"], "error": {"code": -32601, "message": "unsupported method"}})
                        continue
                    result = {"data": [{"turnId": turn["id"], "item": item}
                                       for turn in self.turns for item in turn["items"]] if self.persisted else [],
                              "nextCursor": None}
                if method == "thread/read" and self.delivery:
                    thread = {"id": "thread-old", "archived": self.archived,
                              "status": {"type": "idle" if self.loaded else "notLoaded"}}
                    if msg["params"].get("includeTurns"):
                        thread["turns"] = self.turns if self.persisted else []
                    result = {"thread": thread}
                if msg["method"] == "thread/shellCommand":
                    if self.delivery and not self.loaded:
                        self.reply(conn, {"id": msg["id"], "error": {"code": -32600, "message": "thread not loaded"}})
                        continue
                    # The real server runs the command; capture what it would print.
                    command = msg["params"]["command"]
                    path = pathlib.Path(shlex.split(command)[1])
                    msg["mode"] = path.stat().st_mode & 0o777
                    msg["printed"] = path.read_text(encoding="utf-8")
                    if self.delivery:
                        recorded = "/bin/bash -lc " + shlex.quote(command) if self.wrapped else command
                        item = {"id": "item-one", "type": "commandExecution", "command": recorded, "status": "completed",
                                "exitCode": self.exit_code, "aggregatedOutput": msg["printed"]}
                        self.turns = [{"id": "turn-one", "status": "completed", "items": [item]}]
                        if self.disconnect_before_ack:
                            reader.close()
                            conn.close()
                            return
                        events = [
                            {"method": "item/started", "params": {"threadId": "thread-old", "turnId": "turn-one",
                                                                "item": item}},
                            {"method": "item/completed", "params": {"threadId": "thread-old", "turnId": "turn-one",
                                                                  "item": item}},
                            {"method": "turn/completed", "params": {"threadId": "thread-old",
                                                                  "turn": {"id": "turn-one", "status": "completed"}}},
                        ]
                        if self.wrong:
                            events[-1] = {"method": "turn/completed", "params": {
                                "threadId": "another-thread" if self.wrong == "thread" else "thread-old",
                                "turn": {"id": "another-turn", "status": "completed"}}}
                        if self.early and not self.silent:
                            for event in events:
                                self.reply(conn, event)
                        self.reply(conn, {"id": msg["id"], "result": {}})
                        if not self.early and not self.silent:
                            for event in events:
                                self.reply(conn, event)
                    else:
                        self.reply(conn, {"id": msg["id"], "result": result})
                        self.reply(conn, {"method": "turn/completed", "params": {}})
                else:
                    self.reply(conn, {"id": msg["id"], "result": result})
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

    def saved_receiver(self):
        target = codex_app_notify.state_path()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text('{"thread_id": "thread-old"}', encoding="utf-8")

    def test_deliver_requires_the_existing_unarchived_receiver(self):
        server = FakeAppServer(self.sock, delivery=True, archived=True)
        self.assertEqual(codex_app_notify.deliver(self.repo, "done")["status"], "absent")
        self.saved_receiver()
        receipt = codex_app_notify.deliver(self.repo, "done")
        self.assertEqual(receipt["status"], "archived")
        self.assertNotIn("thread/start", server.methods())
        self.assertNotIn("thread/resume", server.methods())
        self.assertNotIn("thread/shellCommand", server.methods())

    def test_deliver_classifies_deleted_receiver_and_rpc_failure(self):
        self.saved_receiver()
        missing_path = str(self.dir / "missing.sock")
        with mock.patch.dict(os.environ, {"OMS_CODEX_NOTIFY_SOCK": missing_path}):
            missing = FakeAppServer(missing_path, fail={"thread/read"}, delivery=True)
            self.assertEqual(codex_app_notify.deliver(self.repo, "done")["status"], "absent")
            self.assertNotIn("thread/shellCommand", missing.methods())
        failed_path = str(self.dir / "failed.sock")
        with mock.patch.dict(os.environ, {"OMS_CODEX_NOTIFY_SOCK": failed_path}):
            failed = FakeAppServer(failed_path, fail={"thread/shellCommand"}, delivery=True)
            receipt = codex_app_notify.deliver(self.repo, "done")
            self.assertEqual(receipt["status"], "rpc_error")
            self.assertFalse(receipt["acknowledged"])
            self.assertFalse(receipt["delivery_unknown"])
            self.assertEqual(failed.methods().count("thread/shellCommand"), 1)

    def test_deliver_correlates_early_events_and_persisted_output(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, early=True)
        receipt = codex_app_notify.deliver(self.repo, "summary\nrepo/file.py\n")
        self.assertEqual(receipt, {"schema": 1, "status": "persisted", "reason": "",
                                   "thread_id": "thread-old", "turn_id": "turn-one",
                                   "acknowledged": True, "completed": True, "persisted": True,
                                   "submitted": True, "delivery_unknown": False,
                                   "desktop_notification_observed": False})
        self.assertEqual(server.methods().count("thread/shellCommand"), 1)
        self.assertEqual(server.methods().count("thread/read"), 1)
        self.assertEqual(server.methods().count("thread/items/list"), 1)
        self.assertLess(server.methods().index("thread/resume"), server.methods().index("thread/shellCommand"))
        shell = next(call for call in server.calls if call["method"] == "thread/shellCommand")
        self.assertEqual(shell["mode"], 0o600)
        self.assertEqual(list(codex_app_notify.state_path().parent.glob("*.msg")), [])
        self.assertEqual(json.loads(json.dumps(receipt)), receipt)

    def test_deliver_rejects_wrong_events_failed_command_and_missing_history(self):
        self.saved_receiver()
        cases = [(dict(wrong="thread"), "timeout"),
                 (dict(wrong="turn"), "timeout"),
                 (dict(exit_code=1), "failed_command"),
                 (dict(persisted=False), "persistence_failure")]
        for index, (options, expected) in enumerate(cases):
            with self.subTest(options=options):
                path = str(self.dir / (str(index) + ".sock"))
                with mock.patch.dict(os.environ, {"OMS_CODEX_NOTIFY_SOCK": path}):
                    server = FakeAppServer(path, delivery=True, **options)
                    receipt = codex_app_notify.deliver(self.repo, "done", timeout=0.3)
                    self.assertEqual(receipt["status"], expected)
                    self.assertFalse(receipt["persisted"])
                    self.assertEqual(server.methods().count("thread/shellCommand"), 1)

    def test_deliver_bounds_timeout_and_scrubs_message(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, silent=True)
        receipt = codex_app_notify.deliver(self.repo, ("to" + "ken" + "=" + "secret /" + "ho" + "me" + "/user/key"), timeout=0.2)
        self.assertEqual(receipt["status"], "timeout")
        self.assertTrue(receipt["acknowledged"])
        self.assertFalse(receipt["completed"])
        shell = next(call for call in server.calls if call["method"] == "thread/shellCommand")
        self.assertEqual(shell["printed"], "[redacted] [path]\n")
        self.assertEqual(shell["mode"], 0o600)
        self.assertEqual(list(codex_app_notify.state_path().parent.glob("*.msg")), [])

    def test_lost_acknowledgement_leaves_delivery_uncertain(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, disconnect_before_ack=True)
        receipt = codex_app_notify.deliver(self.repo, "done", timeout=0.3)
        self.assertTrue(receipt["submitted"])
        self.assertFalse(receipt["acknowledged"])
        self.assertTrue(receipt["delivery_unknown"])
        self.assertEqual(server.methods().count("thread/shellCommand"), 1)

    def test_deliver_correlates_the_actual_shell_wrapper(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, wrapped=True, early=True)
        self.assertTrue(codex_app_notify.deliver(self.repo, "done")["persisted"])
        self.assertFalse(codex_app_notify._command_matches("bash -lc 'cat result; id'", "cat result"))
        self.assertFalse(codex_app_notify._command_matches("bash -lc 'cat other'", "cat result"))

    def test_deliver_leaves_an_active_receiver_untouched(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, busy=True)
        receipt = codex_app_notify.deliver(self.repo, "done")
        self.assertEqual(receipt["status"], "busy")
        self.assertNotIn("thread/shellCommand", server.methods())

    def test_malformed_receiver_reply_is_a_visible_delivery_failure(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, malformed=True)
        receipt = codex_app_notify.deliver(self.repo, "done")
        self.assertEqual(receipt["status"], "rpc_error")
        self.assertFalse(receipt["submitted"])
        self.assertNotIn("thread/shellCommand", server.methods())

    def test_deliver_falls_back_only_when_pagination_is_unsupported(self):
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True, pagination=False)
        self.assertTrue(codex_app_notify.deliver(self.repo, "done")["persisted"])
        self.assertEqual(server.methods().count("thread/read"), 2)

    def test_delivery_summary_redacts_unlabelled_credentials_and_bounds_utf8(self):
        sample = "gh" + "p_" + "a" * 25
        text = codex_app_notify._safe_message(sample + "\n" + "한" * 2000, self.repo)
        self.assertNotIn(sample, text)
        self.assertLessEqual(len(text.encode("utf-8")), 2400)

    def test_deliver_disabled_does_not_contact_receiver(self):
        self.saved_receiver()
        with mock.patch.dict(os.environ, {"OMS_CODEX_NOTIFY": "0"}):
            self.assertEqual(codex_app_notify.deliver(self.repo, "done")["status"], "disabled")
        self.assertFalse(pathlib.Path(self.sock).exists())

    @unittest.skipUnless(os.name == "posix", "uses the POSIX notification lock")
    def test_deliver_lock_wait_obeys_total_deadline(self):
        import fcntl
        self.saved_receiver()
        server = FakeAppServer(self.sock, delivery=True)
        lock = codex_app_notify.state_path().with_suffix(".lock")
        with lock.open("w") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            receipt = codex_app_notify.deliver(self.repo, "done", timeout=0.1)
        self.assertEqual(receipt["status"], "timeout")
        self.assertNotIn("thread/shellCommand", server.methods())

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
        with mock.patch.dict(os.environ, {"OMS_PANEL_RESULTS": "1"}):
            hook_state.start_codex_notify(self.repo, payload, "panel owner will finalize")
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


@unittest.skipUnless(hasattr(socket, "AF_UNIX") and hasattr(os, "getuid") and os.name != "nt", "Unix inbox transport")
class ClaudeInboxTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="oms-claude-inbox-")
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name).resolve()
        self.registry = self.root / "sessions"
        self.registry.mkdir(mode=0o700)
        self.endpoint = self.root / "receiver.sock"
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(str(self.endpoint))
        self.endpoint.chmod(0o600)
        self.server.listen(1)
        self.server.settimeout(2)
        self.addCleanup(self.server.close)
        self.row = {"sessionId": "fixture-claude-app", "messagingSocketPath": str(self.endpoint),
                    "pid": os.getpid(), "peerProtocol": 1}
        self.record = self.registry / "session.json"
        self.save()
        self.consumer = hashlib.sha256(self.row["sessionId"].encode()).hexdigest()[:32]
        environment = mock.patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": str(self.root), "OMS_CLAUDE_NOTIFY": "1"})
        environment.start()
        self.addCleanup(environment.stop)

    def save(self):
        self.record.write_text(json.dumps(self.row))
        self.record.chmod(0o600)

    def test_bounded_native_post_is_unconfirmed_and_sanitized(self):
        receipt = claude_app_notify.deliver(self.root, "Parser verified\nsec" + "ret=fixture-value\n" + str(self.root),
                                           self.consumer, allow_wakeup=True)
        self.assertEqual(receipt["status"], "submitted")
        self.assertTrue(receipt["delivery_unknown"])
        self.assertFalse(receipt["desktop_notification_observed"])
        with self.server.accept()[0] as connection:
            data = b""
            while b"\n" not in data:
                data += connection.recv(65536)
        frame = json.loads(data)
        self.assertEqual(frame["type"], "user")
        self.assertEqual(frame["priority"], "next")
        self.assertEqual(frame["msg_id"], receipt["message_id"])
        self.assertNotIn("from", frame)
        self.assertNotIn("token", frame)
        content = frame["message"]["content"]
        self.assertIn("not user approval", content)
        self.assertIn("Parser verified", content)
        self.assertNotIn("fixture-value", content)
        self.assertNotIn(str(self.root), content)

    def test_wakeup_is_explicit_and_can_be_disabled(self):
        with mock.patch.object(claude_app_notify, "_registry", side_effect=AssertionError("must not discover")):
            self.assertEqual(claude_app_notify.deliver(self.root, "result", self.consumer)["status"], "wakeup_required")
            with mock.patch.dict(os.environ, {"OMS_CLAUDE_NOTIFY": "0"}):
                self.assertEqual(claude_app_notify.deliver(self.root, "result", self.consumer, True)["status"], "disabled")

    def test_target_never_falls_back_to_latest_and_current_is_exact(self):
        self.record.chmod(0o664)
        with mock.patch.dict(os.environ, {"CLAUDE_CODE_MESSAGING_SOCKET": str(self.endpoint)}):
            self.assertEqual(claude_app_notify.current_session(), self.row["sessionId"])
        self.assertEqual(claude_app_notify.deliver(self.root, "result", "0" * 32, True)["status"], "absent")
        duplicate = self.registry / "duplicate.json"
        duplicate.write_text(self.record.read_text())
        duplicate.chmod(0o600)
        self.assertEqual(claude_app_notify.deliver(self.root, "result", self.consumer, True)["status"], "ambiguous")
        with mock.patch.dict(os.environ, {"CLAUDE_CODE_MESSAGING_SOCKET": str(self.endpoint)}):
            with self.assertRaises(ValueError):
                claude_app_notify.current_session()

    def test_public_or_symlinked_endpoints_are_denied_before_post(self):
        self.registry.chmod(0o755)
        self.assertFalse(claude_app_notify.deliver(self.root, "result", self.consumer, True)["submitted"])
        self.registry.chmod(0o700)
        self.endpoint.chmod(0o666)
        self.assertFalse(claude_app_notify.deliver(self.root, "result", self.consumer, True)["submitted"])
        self.endpoint.chmod(0o600)
        link = self.root / "alias.sock"
        link.symlink_to(self.endpoint)
        self.row["messagingSocketPath"] = str(link)
        self.save()
        self.assertFalse(claude_app_notify.deliver(self.root, "result", self.consumer, True)["submitted"])

    def test_registry_and_connected_process_must_match(self):
        self.row["pid"] = os.getpid() + 1000000
        self.save()
        with mock.patch.object(claude_app_notify.os, "kill", return_value=None):
            if hasattr(socket, "SO_PEERCRED"):
                receipt = claude_app_notify.deliver(self.root, "result", self.consumer, True)
                self.assertEqual(receipt["status"], "unsafe_endpoint")
        self.record.unlink()
        self.record.symlink_to(self.root / "outside.json")
        (self.root / "outside.json").write_text(json.dumps(self.row))
        self.assertEqual(claude_app_notify.deliver(self.root, "result", self.consumer, True)["status"], "absent")

    def test_malformed_oversized_and_future_registry_records_are_ignored(self):
        for content in ('[]', '{', 'x' * 20000, json.dumps(dict(self.row, peerProtocol=2)),
                        json.dumps(dict(self.row, peerProtocol=True)), json.dumps(dict(self.row, pid=True)),
                        json.dumps(dict(self.row, pid=2 ** 1000))):
            with self.subTest(content=content[:60]):
                self.record.write_text(content)
                self.assertEqual(claude_app_notify.deliver(self.root, "result", self.consumer, True)["status"], "absent")
        self.record.unlink()
        os.mkfifo(self.record, 0o600)
        self.assertEqual(claude_app_notify.deliver(self.root, "result", self.consumer, True)["status"], "absent")

    def test_foreign_owner_and_endpoint_swap_are_denied(self):
        uid = os.getuid()
        with mock.patch.object(claude_app_notify.os, "getuid", return_value=uid + 1):
            self.assertFalse(claude_app_notify.deliver(self.root, "result", self.consumer, True)["submitted"])
        original = claude_app_notify._owned_path
        checks = []
        def swapped(path, **kwargs):
            info = original(path, **kwargs)
            if path == self.endpoint:
                checks.append(path)
                if len(checks) == 2:
                    fields = list(info)
                    fields[1] += 1
                    return os.stat_result(fields)
            return info
        with mock.patch.object(claude_app_notify, "_owned_path", side_effect=swapped):
            receipt = claude_app_notify.deliver(self.root, "result", self.consumer, True)
        self.assertEqual(receipt["status"], "unsafe_endpoint")
        self.assertFalse(receipt["submitted"])

    def test_partial_socket_failure_stays_uncertain(self):
        connection = mock.MagicMock()
        connection.__enter__.return_value = connection
        connection.getsockopt.return_value = struct.pack("3i", os.getpid(), os.getuid(), os.getgid())
        connection.sendall.side_effect = socket.timeout("fixture lost response")
        with mock.patch.object(claude_app_notify.socket, "socket", return_value=connection):
            receipt = claude_app_notify.deliver(self.root, "result", self.consumer, True)
        self.assertEqual(receipt["status"], "uncertain")
        self.assertTrue(receipt["delivery_unknown"])

    def test_dead_registry_duplicate_does_not_block_a_resumed_session(self):
        dead = self.registry / "old-process.json"
        dead.write_text(json.dumps(dict(self.row, pid=os.getpid() + 1000000)))
        original = os.kill
        def alive(pid, signal):
            if pid == self.row["pid"]:
                return original(pid, signal)
            raise ProcessLookupError("fixture process exited")
        with mock.patch.object(claude_app_notify.os, "kill", side_effect=alive):
            with mock.patch.dict(os.environ, {"CLAUDE_CODE_MESSAGING_SOCKET": str(self.endpoint)}):
                self.assertEqual(claude_app_notify.current_session(), self.row["sessionId"])
            receipt = claude_app_notify.deliver(self.root, "Resumed receiver", self.consumer, True)
        self.assertEqual(receipt["status"], "submitted")


    def test_incomplete_registry_cannot_establish_a_unique_receiver(self):
        (self.registry / "outside-limit.json").write_text(json.dumps(self.row))
        with mock.patch.object(claude_app_notify, "MAX_REGISTRY", 1):
            self.assertFalse(claude_app_notify.deliver(self.root, "result", self.consumer, True)["submitted"])
            with mock.patch.dict(os.environ, {"CLAUDE_CODE_MESSAGING_SOCKET": str(self.endpoint)}):
                with self.assertRaisesRegex(ValueError, "incomplete"):
                    claude_app_notify.current_session()
        with mock.patch.object(claude_app_notify, "MAX_ENTRIES", 1):
            self.assertFalse(claude_app_notify.deliver(self.root, "result", self.consumer, True)["submitted"])


if __name__ == "__main__":
    unittest.main()
