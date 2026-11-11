"""`oms panel --next-attention`: name, and on request select, the board holding the oldest item waiting on the owner.

One-shot and read-only. The boards are the tmux windows that carry an `@oms_panel_repo` marker (one per
repository and room); each is read with the same snapshot and attention_items the board itself uses, so the
classes and their priority are the panel's own and nothing is re-ranked here. Across boards the oldest stamped
item wins, ties going to the higher-ranked class. `info` rows do not wait on anyone and are skipped. With
`--switch` the tmux client moves to that window through panel_chats.terminal_commands; nothing is answered,
approved, acknowledged or launched, and no key is bound.
"""
import calendar
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

MAX_BOARDS = 16
FORMAT = "#{window_id}\t#{session_id}\t#{pane_id}\t#{@oms_panel_repo}\t#{@oms_panel_room}"
NOTHING = "nothing waiting"


def boards(current_repo):
    """[(repo, room or None, target)] for the known boards; the calling repository alone when tmux is absent."""
    found, seen = [], set()
    if shutil.which("tmux"):
        from panel_state import tmux_command
        try:
            result = subprocess.run(tmux_command("list-windows", "-a", "-F", FORMAT), capture_output=True, text=True,
                                    check=False, timeout=3, stdin=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError):
            result = None
        for line in (result.stdout.replace("\r", "").splitlines() if result and result.returncode == 0 else ()):
            fields = line.split("\t")
            if (len(fields) == 5 and re.fullmatch(r"@[0-9]+", fields[0]) and re.fullmatch(r"\$[0-9]+", fields[1])
                    and re.fullmatch(r"%[0-9]+", fields[2]) and fields[3]):
                key = (fields[3], fields[4])
                if key not in seen and len(found) < MAX_BOARDS:
                    seen.add(key)
                    found.append((fields[3], fields[4] or None, {"window": fields[0], "session": fields[1], "pane": fields[2]}))
    if not found and current_repo is not None:
        found.append((str(current_repo), None, None))
    return found


def _seconds(stamp):
    try:
        return calendar.timegm(time.strptime(str(stamp)[:19], "%Y-%m-%dT%H:%M:%S"))
    except ValueError:
        return None


def waiting(repo, room):
    """(items, readable): this board's attention items, or ([], False) when it cannot be read as a completed read."""
    import panel_attention_read
    import panel_release
    from panel_state import snapshot
    from panel_view import attention_items
    try:
        root = Path(repo).resolve()
        if not root.is_dir():
            return [], False
        state, unused = snapshot(root, room_id=room)
        if state.get("collection", {}).get("ok") is not True or state.get("room", {}).get("error"):
            return [], False
        state["dead_mains"] = panel_release.dead_mains(root)
        state["attention_read"] = panel_attention_read.load(root, room or "")
        return [item for item in attention_items(state, False) if item["cls"] != "info"], True
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        return [], False


def oldest(rows):
    """The (repo, room, target, rank, item) with the oldest stamp; unstamped items only count when none is stamped."""
    candidates = [(row, index, item) for row in rows for index, item in enumerate(row[3])]
    stamped = [(c, _seconds(c[2]["stamp"])) for c in candidates]
    dated = [(c, s) for c, s in stamped if s is not None]
    if dated:
        (row, index, item), unused = min(dated, key=lambda pair: (pair[1], pair[0][1]))
    elif candidates:
        row, index, item = candidates[0]
    else:
        return None
    return row[0], row[1], row[2], item


def run(repo, switch=False):
    from panel_view import age_label
    from terminal_panel import clean
    rows, unreadable = [], 0
    for board_repo, room, target in boards(repo):
        items, readable = waiting(board_repo, room)
        unreadable += not readable
        if items:
            rows.append((board_repo, room, target, items))
    best = oldest(rows)
    if unreadable:
        print("oms panel: %d board%s could not be read" % (unreadable, "" if unreadable == 1 else "s"), file=sys.stderr)
    if best is None:
        print(NOTHING)
        return 0
    board_repo, room, target, item = best
    age = age_label(item["stamp"]) if item["stamp"] else ""
    print(clean("%s: %s %s%s | %s%s" % (item["cls"], item["who"], item["what"], " (%s)" % age if age else "",
                                          Path(board_repo).name, " / " + room if room else ""), 300))
    if not switch:
        return 0
    if target is None:
        print("oms panel: no tmux window holds this board; nothing to switch to", file=sys.stderr)
        return 1
    if not os.environ.get("TMUX"):
        print("oms panel: --switch needs a tmux client; run it inside tmux", file=sys.stderr)
        return 1
    import panel_chats
    try:
        for command in panel_chats.terminal_commands(target):
            subprocess.run(command, capture_output=True, check=True, timeout=3, stdin=subprocess.DEVNULL)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(clean("oms panel: cannot switch: %s" % error, 200), file=sys.stderr)
        return 1
    return 0
