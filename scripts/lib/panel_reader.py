"""The reader pane: one recorded call's full result right of the fixed left board, above the native chat."""

import re
import shutil
import subprocess
import sys
import time

import room
from dashboard_projection import clean
from panel_input import TerminalInput, hit_at
from panel_results import ID

OPTION = "@oms_panel_reader"  # pane option on a reader: the board pane that owns it
BOARD = "OMS_PANEL_READER_BOARD"  # set only in a reader's command; its --results then pages
PANE = re.compile(r"%[0-9]+\Z")
SHARE, MIN_ROWS, MIN_CHAT = 62, 8, 5


def tmux(*args):
    return subprocess.run(["tmux"] + list(args), capture_output=True, text=True, check=False, timeout=3,
                          stdin=subprocess.DEVNULL)


def target_args(target):
    """`oms panel` arguments that read one recorded call; identifiers from recorded state are revalidated here."""
    kind, ident = target
    if kind not in ("result", "debate"):
        raise ValueError("only a recorded call opens in the reader")
    task, who = ident if kind == "debate" else (None, ident)
    room.identifier(who, "participant")
    if task is not None and (not isinstance(task, str) or not ID.fullmatch(task)):
        raise ValueError("invalid task ID")
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


def page(render, board):
    """Scroll `render(width)` until q, Esc or the end of input, or until the board pane is gone (a reader never
    outlives its board); focus then returns to the board."""
    offset, shown, lines, drawn, alive = 0, None, [], None, time.monotonic() + 2
    refresh = 0
    try:
        with TerminalInput() as inputs:
            while True:
                size = tuple(shutil.get_terminal_size())
                if shown != size or time.monotonic() >= refresh:
                    try:
                        updated = render(size[0]).splitlines()
                    except Exception as error:  # the pane stays open to say why instead of vanishing
                        updated = ["Result evidence unavailable: " + clean(str(error), 200)]
                    if updated != lines:
                        drawn = None
                    lines = updated
                    shown = size
                    refresh = time.monotonic() + 2
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
                    offset = (0 if kind == "home" else len(lines) if kind == "end" else offset + (
                        event[1] if kind == "scroll" else {"up": -1, "down": 1, "pageup": -height, "pagedown": height,
                                                           " ": height}.get(kind, 0)))
                if time.monotonic() >= alive:
                    if tmux("display-message", "-p", "-t", board, "#{pane_id}").stdout.strip() != board:
                        return 0
                    alive = time.monotonic() + 2
    finally:
        tmux("select-pane", "-t", board)
