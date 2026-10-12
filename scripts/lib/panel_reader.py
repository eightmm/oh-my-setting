"""The reader pane: one recorded call's full result right of the fixed left board, above the native chat."""

import json
import os
import re
import runpy
import shutil
import subprocess
import sys
import threading
import time
from datetime import datetime
from pathlib import Path

import room
from dashboard_projection import clean
from panel_input import TerminalInput, hit_at
from panel_results import ID

OPTION = "@oms_panel_reader"  # pane option on a reader: the board pane that owns it
BOARD = "OMS_PANEL_READER_BOARD"  # set only in a reader's command; its --results then pages
PANE = re.compile(r"%[0-9]+\Z")
SHARE, MIN_ROWS, MIN_CHAT = 62, 8, 5
VIEWS = ("Result", "Events", "Diff")
PATCH_LIMIT = 65536
EVENT_TAIL_BYTES = 1024 * 1024


def event_tail(repo, attempt_id, maximum=EVENT_TAIL_BYTES):
    """Read a bounded tail of lifecycle events through the ledger's safe descriptor."""
    maximum = max(1, min(int(maximum), EVENT_TAIL_BYTES))
    source = runpy.run_path(str(Path(__file__).with_name("agent-events.py")))
    path = source["event_path"](Path(repo))
    try:
        with source["_ledger_fd"](path) as descriptor:
            size = os.fstat(descriptor).st_size
            start = max(0, size - maximum)
            boundary = False
            if start:
                os.lseek(descriptor, start - 1, os.SEEK_SET)
                boundary = os.read(descriptor, 1) == b"\n"
            os.lseek(descriptor, start, os.SEEK_SET)
            chunks, remaining = [], size - start
            while remaining:
                chunk = os.read(descriptor, min(65536, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
    except FileNotFoundError:
        return [], False
    data = b"".join(chunks)
    if start and not boundary:
        first = data.find(b"\n")
        data = data[first + 1:] if first >= 0 else b""
    raw_lines = data.splitlines()
    if data and not data.endswith(b"\n") and raw_lines:
        raw_lines.pop()
    rows = []
    for line in raw_lines:
        try:
            row = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as error:
            raise ValueError("lifecycle event tail is invalid") from error
        if isinstance(row, dict) and row.get("attempt_id") == attempt_id:
            rows.append(row)
    rows_truncated = len(rows) > 300
    rows = rows[-300:]
    started = any(row.get("event_type") == "attempt.created" for row in rows)
    return rows, bool(not started and (start or rows_truncated))


def render_view(view, report, call, events=(), patch=None, width=100, timeline_truncated=False,
                patch_error=False, heading=None):
    """Render one bounded, read-only view for a recorded call."""
    from panel_view import clipped, render_results, wrapped
    selected = VIEWS.index(view)
    labels = [(("\033[7m" + name + "\033[0m") if i == selected else name)
              for i, name in enumerate(VIEWS)]
    tabs = "  ".join(labels)
    if view == "Result":
        return tabs + "\n" + render_results(report, width, heading=heading or "Call result")
    lines = [tabs]
    if view == "Events":
        def event_order(row):
            stamp = row.get("ts") or row.get("updated_at") or ""
            try:
                stamp = datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()
            except (AttributeError, OSError, OverflowError, TypeError, ValueError):
                stamp = 0
            return stamp, row.get("seq", 0)

        rows = sorted(events, key=event_order)
        heartbeats = 0
        for row in rows:
            kind = str(row.get("event_type", row.get("type", "")))
            if "heartbeat" in kind:
                heartbeats += 1
                continue
            if heartbeats:
                lines.append("  %d heartbeats" % heartbeats)
                heartbeats = 0
            state = row.get("kind") or row.get("to_state") or row.get("state") or row.get("event_type") or kind
            state = {"attempt.created": "started", "attempt.state_changed": "settled"}.get(state, state)
            stamp = row.get("ts") or row.get("updated_at") or "time unavailable"
            try:
                stamp = datetime.fromisoformat(stamp.replace("Z", "+00:00")).astimezone().strftime("%H:%M:%S")
            except (AttributeError, TypeError, ValueError):
                pass
            label = clean(str(state), 120)
            detail = row.get("verdict") or row.get("status") or row.get("outcome")
            summary = row.get("summary")
            if isinstance(detail, dict):
                detail = detail.get("status") or detail.get("outcome")
            if detail:
                label += " · " + clean(str(detail), 120)
            if summary:
                label += " · " + clean(str(summary), 240)
            elif row.get("kind") in ("review", "patch-admit", "patch-land", "panel-delivery", "panel-delivery-pending") and not detail:
                label += " · recorded"
            text = "%s  %s" % (stamp, label)
            lines.extend(wrapped(text, max(8, min(200, width)), 100, max_chars=400))
        if timeline_truncated:
            lines.extend(wrapped("[timeline truncated: call start is outside the retained event window]",
                                 max(8, min(200, width)), 100, max_chars=400))
        if heartbeats:
            lines.append("  %d heartbeats" % heartbeats)
        if len(lines) == 1:
            lines.append("No retained events for this call.")
        return "\n".join(lines)
    if not patch:
        lines.append("patch unreadable (digest mismatch / read error)" if patch_error else "no patch")
        return "\n".join(lines)
    content = patch[:PATCH_LIMIT].decode("utf-8", "replace")
    for line in content.splitlines():
        line = clipped(line, max(8, width))
        if line.startswith(("diff --git ", "--- ", "+++ ")):
            lines.append("\033[1m" + line + "\033[0m")
        elif line.startswith("+"):
            lines.append("\033[32m" + line + "\033[0m")
        elif line.startswith("-"):
            lines.append("\033[31m" + line + "\033[0m")
        else:
            lines.append(line)
    if len(patch) > PATCH_LIMIT:
        lines.append("[patch capped at %d bytes]" % PATCH_LIMIT)
    return "\n".join(lines)


def tmux(*args):
    return subprocess.run(["tmux"] + list(args), capture_output=True, text=True, check=False, timeout=3,
                          stdin=subprocess.DEVNULL)


def target_args(target):
    """Read-only `oms panel` arguments; identifiers from recorded state are revalidated here."""
    kind, ident = target
    if kind not in ("result", "debate", "mail"):
        raise ValueError("only recorded results or main messages open in the reader")
    task, who = ident if kind == "debate" else (None, ident)
    room.identifier(who, "participant")
    if task is not None and (not isinstance(task, str) or not ID.fullmatch(task)):
        raise ValueError("invalid task ID")
    if kind == "mail":
        return ["--mail", who]
    return ["--results", "--participant", who] + (["--task-id", task] if task else [])


def picked(event, navigation):
    """The call or main a click or Enter just chose on the board (it previews there), else None."""
    target = (navigation.get("preview") or {}).get("target")
    if event[0] == "click":
        chosen = (hit_at(event, navigation) or {}).get("action")
    else:
        chosen = navigation.get("selected") if event[0] == "enter" else None
    return target if target and chosen == target and target[0] in ("result", "debate", "chat") else None


def split_rows(height):
    """Reader rows taken from a chat pane `height` rows tall; None when the reader or the chat would be too small."""
    rows = height * SHARE // 100
    return rows if rows >= MIN_ROWS and height - rows - 1 >= MIN_CHAT else None


def window_panes(board):
    """{pane: (height, owning board)} for the board's window."""
    result = tmux("list-panes", "-t", board, "-F", "#{pane_id}\t#{pane_height}\t#{" + OPTION + "}")
    panes = {}
    for line in result.stdout.replace("\r", "").splitlines() if result.returncode == 0 else []:
        fields = line.split("\t")
        if len(fields) == 3 and PANE.fullmatch(fields[0]) and fields[1].isdigit():
            panes[fields[0]] = (int(fields[1]), fields[2])
    return panes


def find(board, panes=None):
    panes = window_panes(board) if panes is None else panes
    return next((pane for pane, (unused, owner) in panes.items() if owner == board and pane != board), None)


def open_reader(board, command, cwd):
    """Run `command` in the board's reader pane and return the pane: an existing reader is respawned, otherwise the
    native chat pane is split with the reader on top. None when there is no recorded chat pane or too little room;
    the chat pane itself is never respawned or killed."""
    if not PANE.fullmatch(board or ""):
        return None
    panes = window_panes(board)
    pane = find(board, panes)
    if pane:
        return None if tmux("respawn-pane", "-k", "-t", pane, "-c", str(cwd), command).returncode else pane
    native = tmux("show-option", "-w", "-v", "-t", board, "@oms_panel_native_pane").stdout.strip()
    if not PANE.fullmatch(native) or native == board or native not in panes or not split_rows(panes[native][0]):
        return None
    made = tmux("split-window", "-d", "-v", "-b", "-l", str(split_rows(panes[native][0])), "-t", native,
                "-c", str(cwd), "-P", "-F", "#{pane_id}", command)
    pane = made.stdout.strip()
    if made.returncode or not PANE.fullmatch(pane):
        return None
    marked = False
    try:
        marked = tmux("set-option", "-p", "-t", pane, OPTION, board).returncode == 0
    finally:
        if not marked:
            try:
                tmux("kill-pane", "-t", pane)
            except (OSError, subprocess.SubprocessError):
                pass
    return pane if marked else None


def close_reader(board):
    """Close the board's reader pane; True when one was open."""
    pane = find(board) if PANE.fullmatch(board or "") else None
    if pane:
        tmux("kill-pane", "-t", pane)
    return bool(pane)


def page(render, board, on_refresh=None):
    """Scroll `render(width)` until q, Esc or the end of input, or until the board pane is gone (a reader never
    outlives its board); focus then returns to the board."""
    offset, shown, lines, drawn, alive = 0, None, ["Reading the recorded result..."], None, time.monotonic() + 2
    view = "Result"
    refresh, read = 0, {}

    def fetch(width):
        # One read at a time off the input thread: a slow result query never blocks q/Esc or scrolling.
        try:
            if on_refresh is not None:
                on_refresh()
            try:
                value = render(width, view)
            except TypeError:
                value = render(width)
            read["lines"] = (width, value.splitlines())
        except Exception as error:  # the pane stays open to say why instead of vanishing
            read["lines"] = (width, ["Result evidence unavailable: " + clean(str(error), 200)])

    try:
        with TerminalInput(extra_keys=("r", "e", "d")) as inputs:
            while True:
                size = tuple(shutil.get_terminal_size())
                running = read.get("thread") and read["thread"].is_alive()
                if not running and (shown != size[0] or time.monotonic() >= refresh):
                    read["thread"] = threading.Thread(target=fetch, args=(size[0],), daemon=True)
                    read["thread"].start()
                    read["thread"].join(.2)
                    shown, refresh = size[0], time.monotonic() + 2
                if "lines" in read:
                    width, updated = read.pop("lines")
                    if width == size[0] and updated != lines:
                        lines, drawn = updated, None
                height = max(1, size[1] - 1)
                offset = max(0, min(offset, len(lines) - height))
                if drawn != (offset, size, len(lines)):
                    footer = "q/Esc close  Up/Down PgUp/PgDn scroll  %d-%d/%d" % (
                        offset + 1, min(len(lines), offset + height), len(lines))
                    sys.stdout.write("\033[H\033[2J" + "\n".join(lines[offset:offset + height] + [""] * (
                        height - len(lines[offset:offset + height]))) + "\n" + footer[:size[0]])
                    sys.stdout.flush()
                    drawn = (offset, size, len(lines))
                for event in inputs.wait(1):
                    kind = event[0]
                    if kind in ("q", "escape", "eof"):
                        return 0
                    if kind == "repaint":
                        shown = drawn = None
                    if kind in ("tab", "r", "e", "d"):
                        view = {"r": "Result", "e": "Events", "d": "Diff"}.get(
                            kind, VIEWS[(VIEWS.index(view) + 1) % len(VIEWS)])
                        offset, shown, drawn = 0, None, None
                    offset = (0 if kind == "home" else len(lines) if kind == "end" else offset + (
                        event[1] if kind == "scroll" else {"up": -1, "down": 1, "pageup": -height, "pagedown": height,
                                                           " ": height}.get(kind, 0)))
                if time.monotonic() >= alive:
                    if tmux("display-message", "-p", "-t", board, "#{pane_id}").stdout.strip() != board:
                        return 0
                    alive = time.monotonic() + 2
    finally:
        tmux("select-pane", "-t", board)
