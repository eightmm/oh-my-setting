#!/usr/bin/env python3
"""Contract tests for the Codex turn -> Claude terminal notifier."""

import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "lib" / "codex_turn_notify.py"


class CodexTurnNotifyTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        self.bin = self.dir / "bin"
        self.bin.mkdir()
        self.repo = self.dir / "work" / "proj"
        self.repo.mkdir(parents=True)
        # Two attached clients; the older one shows Claude in this project.
        self.near, self.far = self.dir / "tty-near", self.dir / "tty-far"
        tmux = self.bin / "tmux"
        tmux.write_text(
            "#!/bin/sh\n"
            "case \"$1\" in\n"
            "  list-clients) printf '%s\\t100\\tproj\\n%s\\t200\\tother\\n' '" + str(self.near) + "' '" + str(self.far) + "' ;;\n"
            "  list-panes) printf 'proj\\tclaude\\t%s\\nother\\tbash\\t/\\n' '" + str(self.repo) + "' ;;\n"
            "esac\n", encoding="utf-8")
        tmux.chmod(tmux.stat().st_mode | stat.S_IEXEC)
        self.codex = self.dir / "codex"
        (self.codex / "sessions" / "2026").mkdir(parents=True)
        self.state = self.dir / "state"

    def tearDown(self):
        self.tmp.cleanup()

    def rollout(self, thread, source):
        (self.codex / "sessions" / "2026" / ("rollout-x-%s.jsonl" % thread)).write_text(
            json.dumps({"type": "session_meta", "payload": {"source": source}}) + "\n", encoding="utf-8")

    def run_notify(self, event, **env):
        full = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ.get("PATH", ""),
                    CODEX_HOME=str(self.codex), XDG_STATE_HOME=str(self.state), **env)
        full.pop("OMS_HARNESS_CHILD", None)
        full.update(env)
        subprocess.run([sys.executable, str(SCRIPT), json.dumps(event)], env=full, check=True, timeout=30)

    def event(self, **extra):
        body = {"type": "agent-turn-complete", "thread-id": "t-main", "cwd": str(self.repo),
                "last-assistant-message": "Tests pass.\x1b]0;evil\x07"}
        body.update(extra)
        return body

    def test_notifies_the_claude_terminal_of_this_project_once(self):
        self.rollout("t-main", "vscode")
        self.run_notify(self.event())
        data = self.near.read_bytes()
        self.assertTrue(data.startswith(b"\x1b]9;"), data)
        self.assertTrue(data.endswith(b"\x07"), data)
        self.assertEqual(data.count(b"\x07"), 1, "an escape in the answer must not end the OSC early")
        self.assertIn("Codex 작업 완료 · proj".encode(), data)
        self.assertIn(b"Tests pass.", data)
        self.assertFalse(self.far.exists(), "one notification, not one per terminal")

    def test_skips_what_is_not_the_users_chat(self):
        self.rollout("t-guard", {"subagent": {"other": "guardian"}})
        self.rollout("t-exec", "exec")
        self.state.joinpath("oh-my-setting").mkdir(parents=True)
        self.state.joinpath("oh-my-setting", "codex-notify.json").write_text('{"thread_id": "t-notify"}')
        for thread in ("t-guard", "t-exec", "t-notify"):
            self.run_notify(self.event(**{"thread-id": thread}))
        self.run_notify(self.event(type="other"))
        self.run_notify(self.event(), OMS_HARNESS_CHILD="1")
        self.run_notify(self.event(), OMS_TERMINAL_NOTIFY="0")
        self.assertFalse(self.near.exists())
        self.assertFalse(self.far.exists())


if __name__ == "__main__":
    unittest.main()
