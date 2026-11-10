"""New input surfaces: real temporary Git/room logs and fake read-only tmux.

Room transport and receipts are real; no provider CLI or external message service
is invoked. Existing room protocol tests retain their own contract coverage.
"""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
ENTRY = ROOT / "scripts" / "input.sh"
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
import room
from oms_inputs import queue
from oms_runtime.common import CoreError, append_jsonl, atomic_write_json, canonical_json, sha256_bytes


FAKE_TMUX = r'''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
with open(os.environ["OMS_FAKE_TMUX_LOG"], "a") as f:
    f.write(json.dumps(a) + "\n")
assert a[0] in ("display-message", "capture-pane"), "no terminal input is allowed"
assert a[a.index("-t") + 1] == "%7"
if a[0] == "display-message":
    print("%7\t" + os.environ.get("OMS_FAKE_PANE_PID", "12345") + "\t" + os.environ["OMS_FAKE_PANE_CWD"] + "\t0")
else:
    print(os.environ.get("OMS_FAKE_CAPTURE", "old line\nselected output\nlast line"))
'''


class InputsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="oms-input-test.")
        self.base = Path(self.temp.name).resolve()
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.home = self.base / "home"
        self.home.mkdir()
        self.bin = self.base / "bin"
        self.bin.mkdir()
        fake = self.bin / "tmux"
        fake.write_text(FAKE_TMUX)
        fake.chmod(0o700)
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        OMS_LOCK_DIR=str(self.base / "locks"), OMS_FAKE_TMUX_LOG=str(self.base / "tmux.jsonl"),
                        OMS_FAKE_PANE_CWD=str(self.repo), OMS_HARNESS_CHILD="0", OMS_HARNESS_DELEGATE_DEPTH="0",
                        PYTHONDONTWRITEBYTECODE="1")
        for key in ("OMS_ROOM_REPO", "OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            self.env.pop(key, None)
        self.env_patch = patch.dict(os.environ, self.env, clear=True)
        self.env_patch.start()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Input Fixture")
        self.git("config", "user.email", "input@example.invalid")
        (self.repo / "source.py").write_text("first line\nselected line\nthird line\n")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        room.create(self.repo, "input-room", "Input fixture", creator="main")
        room.join(self.repo, "input-room", "main", "codex", native_session="main-session")
        room.join(self.repo, "input-room", "worker", "claude", role="worker", parent="main")

    def tearDown(self):
        self.env_patch.stop()
        self.temp.cleanup()

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], env=self.env,
                              capture_output=True, check=True).stdout

    def cli(self, *args, env=None, ok=True):
        result = subprocess.run(["bash", str(ENTRY), "--repo", str(self.repo), *map(str, args)],
                                env=dict(self.env, **(env or {})), cwd=self.repo, capture_output=True, text=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def route(self):
        return ["--room", "input-room", "--from", "main", "--to", "worker"]

    def add(self, text="Please inspect the selected source", mode="next", attachment=None):
        extra = ["--attachment", attachment] if attachment else []
        return self.cli("add", *self.route(), "--mode", mode, "--text", text, *extra)

    def messages(self):
        return room.status(self.repo, "input-room")["messages"]

    def imported_feedback(self, *, next_text="Evaluate the selected candidates", row_key="molecule-1", action="select"):
        view = {"schema": 1, "id": "view-fixture", "data_sha256": "d" * 64, "title": "Candidates",
                "row_keys": ["molecule-1", "molecule-2"], "created_at": "2026-11-10T00:00:00Z",
                "path": "view.html", "html_sha256": "a" * 64, "molecule_js_sha256": "b" * 64}
        body = {"view_id": view["id"], "data_sha256": view["data_sha256"],
                "events": [{"row_key": row_key, "action": action, "comment": "Chosen for follow-up"}],
                "report": {"title": "Review", "question": "Which candidate?", "decision": "Choose one", "next": next_text}}
        batch = sha256_bytes(canonical_json(body))
        folder = self.repo / ".oms" / "artifact-views"
        atomic_write_json(folder / (view["id"] + ".json"), view)
        append_jsonl(folder / "feedback.jsonl", dict(body, schema=1, batch_id=batch, created_at="2026-11-10T00:00:00Z"))
        return batch, view

    def test_file_attachment_preserves_exact_lines_full_digest_and_drift(self):
        before = self.git("status", "--porcelain")
        attached = self.cli("attach", "file", "source.py", "--start-line", 2, "--end-line", 2)
        self.assertEqual(attached["source"]["full_file_sha256"], sha256_bytes((self.repo / "source.py").read_bytes()))
        opened = self.cli("attachment", attached["id"])
        self.assertEqual(opened["content"], "selected line\n")
        self.assertEqual(opened["source_status"], "current")
        self.assertEqual(self.git("status", "--porcelain"), before)
        record = self.repo / ".oms" / "inputs" / "attachments" / attached["id"] / "meta.json"
        self.assertEqual(record.stat().st_mode & 0o777, 0o600)
        queued = self.add(attachment=attached["id"])
        (self.repo / "source.py").write_text("changed outside selection\nselected line\nthird line\n")
        self.assertEqual(self.cli("attachment", attached["id"])["source_status"], "drifted-or-unavailable")
        failed = self.cli("deliver", queued["id"], "--apply", "--at-boundary", ok=False)
        self.assertIn("drifted", failed.stderr)
        self.assertEqual(self.messages(), [])
        self.assertEqual(self.cli("show", queued["id"])["status"], "queued")

    def test_selection_is_declared_unverified_and_terminal_capture_is_read_only(self):
        selected = self.cli("attach", "selection", "--source", "browser", "--origin", "https://example.org/paper",
                            "--text", "Explicitly selected paragraph")
        self.assertFalse(selected["source"]["verified"])
        terminal = self.cli("attach", "terminal", "--pane", "%7", "--lines", 2)
        self.assertEqual(self.cli("attachment", terminal["id"])["content"], "selected output\nlast line\n")
        calls = [json.loads(line) for line in Path(self.env["OMS_FAKE_TMUX_LOG"]).read_text().splitlines()]
        self.assertTrue(all(row[0] in ("display-message", "capture-pane") for row in calls))
        item = self.add(attachment=terminal["id"])
        self.cli("deliver", item["id"], "--apply", "--at-boundary", env={"OMS_FAKE_PANE_PID": "54321"}, ok=False)
        self.assertEqual(self.messages(), [])
        self.cli("attach", "terminal", "--pane", "%7", env={"OMS_FAKE_PANE_CWD": str(self.base)}, ok=False)

    def test_edit_reorder_cancel_boundary_and_exact_preview_digest(self):
        first, second = self.add("first task"), self.add("second task")
        self.cli("edit", second["id"], "--text", "edited second task")
        self.assertIn("earlier", self.cli("deliver", second["id"], "--apply", "--at-boundary", ok=False).stderr)
        self.cli("move", second["id"], "--before", first["id"])
        before = self.cli("preview", second["id"])
        self.assertEqual(self.cli("deliver", second["id"])["payload_sha256"], before["payload_sha256"])
        self.assertIn("--at-boundary", self.cli("deliver", second["id"], "--apply", ok=False).stderr)
        self.cli("deliver", second["id"], "--apply", "--at-boundary", "--expected-sha256", "0" * 64, ok=False)
        delivered = self.cli("deliver", second["id"], "--apply", "--at-boundary", "--expected-sha256", before["payload_sha256"])
        self.assertEqual(delivered["status"], "delivered")
        self.assertEqual(self.messages()[0]["text"], before["text"])
        self.cli("edit", second["id"], "--text", "too late", ok=False)
        self.cli("cancel", first["id"])
        self.assertEqual(self.cli("show", first["id"])["status"], "cancelled")
        self.assertTrue(self.cli("deliver", second["id"], "--apply")["deduplicated"])
        self.assertEqual(len(self.messages()), 1)

    def test_attachment_delivery_contains_reference_not_snapshot_and_receipt(self):
        attached = self.cli("attach", "file", "source.py")
        row = self.add(mode="steer", attachment=attached["id"])
        delivered = self.cli("deliver", row["id"], "--apply")
        message = self.messages()[0]
        self.assertIn("oms input attachment " + attached["id"], message["text"])
        self.assertIn(attached["content_sha256"], message["text"])
        self.assertNotIn("selected line", message["text"])
        self.assertLessEqual(len(message["text"].encode()), 4000)
        self.assertFalse(delivered["receipt"]["acknowledged"])
        room.acknowledge(self.repo, "input-room", "worker", [row["message_id"]])
        self.assertTrue(self.cli("reconcile", row["id"])["receipt"]["acknowledged"])

    def test_uncertain_append_after_effect_reconciles_without_duplicate(self):
        row = self.add(mode="steer")
        original = room.send
        def lost_receipt(*args, **kwargs):
            original(*args, **kwargs)
            raise ValueError("simulated response loss after append")
        with patch.object(room, "send", lost_receipt):
            uncertain = queue.deliver(self.repo, row["id"], apply=True)
        self.assertEqual(uncertain["status"], "uncertain")
        self.assertEqual(len(self.messages()), 1)
        self.cli("deliver", row["id"], "--apply", ok=False)
        recovered = self.cli("reconcile", row["id"])
        self.assertEqual(recovered["status"], "delivered")
        self.assertFalse(recovered["retry_allowed"])
        self.assertEqual(len(self.messages()), 1)

    def test_uncertain_absence_and_conflicting_identity_never_blindly_retry(self):
        row = self.add(mode="steer")
        with patch.object(room, "send", side_effect=ValueError("simulated transport failure")):
            queue.deliver(self.repo, row["id"], apply=True)
        response = self.cli("reconcile", row["id"], ok=False)
        self.assertEqual(response.returncode, 3)
        self.assertFalse(json.loads(response.stdout)["observed"])
        self.cli("deliver", row["id"], "--apply", ok=False)
        self.cli("cancel", row["id"], ok=False)
        room.send(self.repo, "input-room", "main", "worker", "different payload", message_id=row["message_id"])
        self.assertIn("different content", self.cli("reconcile", row["id"], ok=False).stderr)

    def test_identity_change_during_append_cannot_claim_confirmed_delivery(self):
        row = self.add(mode="steer")
        original = room.send
        def raced(*args, **kwargs):
            room.append(self.repo, "input-room", {"kind": "leave", "participant": "worker"}, "Left")
            room.join(self.repo, "input-room", "worker", "claude", role="worker", parent="main")
            return original(*args, **kwargs)
        with patch.object(room, "send", raced):
            attempted = queue.deliver(self.repo, row["id"], apply=True)
        self.assertEqual(attempted["status"], "uncertain")
        self.assertEqual(len(self.messages()), 1)
        self.assertIn("different participant", self.cli("reconcile", row["id"], ok=False).stderr)
        self.cli("deliver", row["id"], "--apply", ok=False)

    def test_rollover_preserves_addressing_lineage_and_historical_receipts(self):
        row = self.add(mode="steer")
        room.rollover(self.repo, "input-room")
        preview = self.cli("preview", row["id"])
        self.assertNotEqual(preview["room"], "input-room")
        self.cli("deliver", row["id"], "--apply")
        room.acknowledge(self.repo, "input-room", "worker", [row["message_id"]])
        room.rollover(self.repo, room.resolve(self.repo, "input-room", fresh=True))
        reconciled = self.cli("reconcile", row["id"])
        self.assertEqual(reconciled["status"], "delivered")
        self.assertTrue(reconciled["receipt"]["acknowledged"])

    def test_source_target_or_owner_changes_fail_before_room_send(self):
        row = self.add(mode="steer")
        room.append(self.repo, "input-room", {"kind": "leave", "participant": "worker"}, "Left")
        room.join(self.repo, "input-room", "worker", "claude", role="worker", parent="main")
        self.assertIn("changed", self.cli("deliver", row["id"], "--apply", ok=False).stderr)
        self.assertEqual(self.messages(), [])
        fresh = self.add(mode="steer")
        old = room.participant(room.status(self.repo, "input-room"), "main")["consumer"]
        room.bind(self.repo, "input-room", "main", "different-main-session", replaces=old)
        self.cli("deliver", fresh["id"], "--apply", ok=False)
        self.assertEqual(self.messages(), [])

    def test_concurrent_delivery_has_one_durable_message(self):
        row = self.add(mode="steer")
        argv = ["bash", str(ENTRY), "--repo", str(self.repo), "deliver", row["id"], "--apply"]
        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _: subprocess.run(argv, env=self.env, cwd=self.repo,
                                capture_output=True, text=True), range(2)))
        self.assertTrue(all(result.returncode == 0 for result in results), [r.stderr for r in results])
        self.assertEqual(len(self.messages()), 1)

    def test_feedback_adapter_binds_digest_selections_and_idempotent_mapping(self):
        batch, view = self.imported_feedback()
        result = self.cli("feedback", "--batch", batch, *self.route())
        row = result["input"]
        self.assertEqual(row["provenance"]["batch_id"], batch)
        self.assertEqual(row["provenance"]["selected_rows"], ["molecule-1"])
        self.assertIn("Evaluate the selected candidates", row["text"])
        body = self.cli("attachment", row["attachments"][0]["id"], "--full")
        self.assertEqual(json.loads(body["content"])["events"][0]["row_key"], "molecule-1")
        again = self.cli("feedback", "--batch", batch, *self.route())
        self.assertTrue(again["deduplicated"])
        self.assertEqual(again["input"]["id"], row["id"])
        self.assertEqual(self.messages(), [])
        view["data_sha256"] = "e" * 64
        atomic_write_json(self.repo / ".oms" / "artifact-views" / "view-fixture.json", view)
        self.cli("deliver", row["id"], "--apply", "--at-boundary", ok=False)
        self.assertEqual(self.messages(), [])

    def test_feedback_invalid_rows_and_overlong_next_are_not_truncated_or_queued(self):
        batch, _ = self.imported_feedback(row_key="not-in-view")
        self.cli("feedback", "--batch", batch, *self.route(), ok=False)
        batch, _ = self.imported_feedback(next_text="x" * 4000)
        self.assertIn("budget", self.cli("feedback", "--batch", batch, *self.route(), ok=False).stderr)
        self.assertEqual(self.cli("list")["items"], [])

    def test_worker_secret_symlink_and_size_guards(self):
        selected = self.cli("attach", "file", "source.py")
        child = {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_REPO": str(self.repo), "OMS_ROOM_ID": "input-room",
                 "OMS_ROOM_PARTICIPANT": "worker"}
        self.cli("attachment", selected["id"], env=child, ok=False)
        sent = self.add(mode="steer", attachment=selected["id"])
        self.cli("deliver", sent["id"], "--apply")
        self.assertEqual(self.cli("attachment", selected["id"], env=child)["id"], selected["id"])
        for action in (("attachments",), ("list",), ("show", sent["id"]), ("preview", sent["id"]),
                       ("deliver", sent["id"])):
            self.cli(*action, env=child, ok=False)
        self.cli("attach", "file", "source.py", env=child, ok=False)
        self.cli("add", *self.route(), "--text", "unauthorized", env=child, ok=False)
        self.cli("add", *self.route(), "--text", "api_" "key=DO_NOT_FORWARD", ok=False)
        self.cli("add", *self.route(), "--text", '{"api_' 'key":"DO_NOT_FORWARD"}', ok=False)
        self.cli("attach", "selection", "--source", "browser", "--origin", "https://example.org/?tok" "en=private",
                 "--text", "selected text", ok=False)
        (self.repo / "link").symlink_to(self.repo / "source.py")
        self.cli("attach", "file", "link", ok=False)
        (self.repo / "large.txt").write_text("x" * (64 * 1024 + 1))
        self.cli("attach", "file", "large.txt", ok=False)
        self.cli("attach", "file", "../outside", ok=False)
        content = self.repo / ".oms" / "inputs" / "attachments" / selected["id"] / "content.txt"
        content.write_text("tampered")
        self.assertIn("changed", self.cli("attachment", selected["id"], ok=False).stderr)

    def test_worker_cannot_read_another_recipient_or_rejoined_identity_attachment(self):
        selected = self.cli("attach", "file", "source.py")
        room.join(self.repo, "input-room", "other-worker", "claude", role="worker", parent="main")
        sent = self.add(mode="steer", attachment=selected["id"])
        self.cli("deliver", sent["id"], "--apply")
        inherited = {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_REPO": str(self.repo), "OMS_ROOM_ID": "input-room"}
        self.cli("attachment", selected["id"], env=dict(inherited, OMS_ROOM_PARTICIPANT="other-worker"), ok=False)
        room.append(self.repo, "input-room", {"kind": "leave", "participant": "worker"}, "Left")
        room.join(self.repo, "input-room", "worker", "claude", role="worker", parent="main")
        self.cli("attachment", selected["id"], env=dict(inherited, OMS_ROOM_PARTICIPANT="worker"), ok=False)

    def test_ambient_git_redirects_are_refused_without_state_mutation(self):
        before = self.git("status", "--porcelain")
        for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
            failed = self.cli("attach", "file", "source.py", env={key: str(self.base / "foreign")}, ok=False)
            self.assertIn("redirect", failed.stderr)
        self.assertFalse((self.repo / ".oms" / "inputs").exists())
        self.assertEqual(self.git("status", "--porcelain"), before)


if __name__ == "__main__":
    unittest.main()
