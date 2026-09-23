#!/usr/bin/env python3
"""Codex `notify` target: show a finished Codex turn in the Claude Code terminal.

Codex runs this with its turn-complete JSON as the last argument, from the
app-server daemon's environment, so nothing here trusts PATH or $TMUX. The
notification is an OSC 9 escape written to the tmux *client* terminal (not a
pane, so tmux passthrough is not involved); the terminal app on the other end
of SSH turns it into an OS notification. Best-effort: every failure exits 0.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

# The daemon's PATH may be minimal; common install locations back it up.
SEARCH_PATH = os.pathsep.join(
    [os.environ.get("PATH", ""), "/usr/local/bin", "/usr/bin", "/bin", "/opt/homebrew/bin",
     str(Path.home() / ".local" / "bin")])


def rollout_source(thread: str) -> object:
    """session_meta.source of the thread's rollout, or None when not found."""
    sessions = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex") / "sessions"
    for folder, _, names in os.walk(sessions):
        for name in names:
            if thread in name and name.endswith(".jsonl"):
                try:
                    with open(os.path.join(folder, name), "rb") as handle:
                        return (json.loads(handle.readline(262144)).get("payload") or {}).get("source")
                except (OSError, ValueError, AttributeError):
                    return None
    return None


def should_notify(event: dict) -> bool:
    if event.get("type") != "agent-turn-complete" or not event.get("last-assistant-message"):
        return False
    thread = str(event.get("thread-id") or "")
    import codex_app_notify

    # The Claude notification chat's own turns would bounce straight back.
    if thread and thread == codex_app_notify.thread_id():
        return False
    source = rollout_source(thread) if thread else None
    # Guardian/reviewer threads and headless exec runs are not the user's chat.
    return not (source == "exec" or (isinstance(source, dict) and "subagent" in source))


def tmux(*args: str) -> list:
    binary = shutil.which("tmux", path=SEARCH_PATH)
    if not binary:
        return []
    try:
        out = subprocess.run([binary, *args], capture_output=True, text=True, timeout=3, check=False)
    except (OSError, subprocess.SubprocessError):
        return []
    return [line.split("\t") for line in out.stdout.splitlines() if line]


def target_ttys(cwd: str) -> list:
    """One terminal: the client showing a Claude pane in this project, else the latest client."""
    clients = [row for row in tmux("list-clients", "-F", "#{client_tty}\t#{client_activity}\t#{session_name}")
               if len(row) == 3]
    if clients:
        here = Path(cwd).resolve() if cwd else None
        claude_sessions = set()
        for row in tmux("list-panes", "-a", "-F", "#{session_name}\t#{pane_current_command}\t#{pane_current_path}"):
            if len(row) == 3 and row[1] == "claude" and here is not None:
                pane = Path(row[2]).resolve()
                if pane == here or here in pane.parents or pane in here.parents:
                    claude_sessions.add(row[0])
        pool = [row for row in clients if row[2] in claude_sessions] or clients
        return [max(pool, key=lambda row: int(row[1]) if row[1].isdigit() else 0)[0]]
    # No tmux: the terminal a Claude Code process runs in.
    try:
        out = subprocess.run(["ps", "-axo", "tty=,comm="], capture_output=True, text=True, timeout=3, check=False)
    except (OSError, subprocess.SubprocessError):
        return []
    for line in out.stdout.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2 and Path(parts[1].strip()).name == "claude" and parts[0] not in ("?", "??"):
            return ["/dev/" + parts[0]]
    return []


def escape(title: str, body: str) -> bytes:
    from work_journal import sanitize_text

    text = sanitize_text("%s: %s" % (title, body), 240)
    # OSC payloads end at BEL/ESC; sanitize_text already removed controls.
    text = re.sub(r"[\x00-\x1f\x7f]", " ", text)
    title = re.sub(r"[\x00-\x1f\x7f;]", " ", title)
    if os.environ.get("OMS_TERMINAL_NOTIFY_OSC") == "777":
        return ("\033]777;notify;%s;%s\a" % (title, text)).encode("utf-8")
    return ("\033]9;%s\a" % text).encode("utf-8")


def main() -> int:
    if os.environ.get("OMS_TERMINAL_NOTIFY", "1") != "1" or os.environ.get("OMS_HARNESS_CHILD") == "1":
        return 0
    try:
        event = json.loads(sys.argv[-1])
    except (IndexError, ValueError):
        return 0
    if not isinstance(event, dict) or not should_notify(event):
        return 0
    cwd = str(event.get("cwd") or "")
    data = escape("🔔 Codex 작업 완료 · %s" % (Path(cwd).name or "codex"),
                  str(event.get("last-assistant-message") or "")[-1000:])
    for tty in target_ttys(cwd):
        try:
            with open(tty, "wb", buffering=0) as handle:
                handle.write(data)
        except OSError:
            continue
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        raise SystemExit(0)
