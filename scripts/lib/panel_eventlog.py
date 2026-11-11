"""Append-only event log for `oms panel --log [--follow]`: one plain line per room member state, no repaint.

Line shape (fixed words, single spaces, so grep and awk stay stable):

    HH:MM:SS room=<room id> role=<role> <old> -> <new> <title>

`<old>` is `-` for a snapshot line and for a member that appeared. No cursor movement, colour or glyphs are
written; titles lose control bytes and are bounded, and nothing else from a call (prompt, body, path) is read.
Change detection is `panel_watch.changed_rows`, the watch loop's own differ. A foreground command, not a daemon."""

import os
import sys
import time

TITLE_CHARS = 80


def text(value, limit):
    """One printable line: control bytes (including ESC) become spaces, runs of space collapse, bounded."""
    plain = " ".join("".join(ch if ch.isprintable() else " " for ch in str(value or "")).split())
    return plain if len(plain) <= limit else plain[:limit - 1] + "~"


def word(value, limit):
    return text(value, limit).replace(" ", "_")


def line(now, room_id, member, old=None):
    return "%s room=%s role=%s %s -> %s %s" % (
        time.strftime("%H:%M:%S", time.localtime(now)), word(room_id, 64) or "-", word(member.get("role"), 32) or "-",
        word(old, 32) or "-", word(member.get("state"), 32) or "unknown", text(member.get("title"), TITLE_CHARS) or "-")


def lines(report, known, key, now, announced):
    """(lines, known, announced) for one completed read.

    The first readable read lists every member as a snapshot; later reads list only members `changed_rows`
    reports, one line each however many ids the member answers to. A blind read prints nothing and forgets what
    it knew, so the read after it seeds again rather than reporting what moved while it could not see."""
    import panel_watch
    import room_view
    from dashboard_projection import mapping
    from panel_view import freeze_report
    board = freeze_report(report)
    fresh, seen = panel_watch.changed_rows(known, board, key)
    if seen is None:
        return [], None, announced
    room_id = mapping(board.get("room")).get("id")
    members = room_view.nodes(board)
    if not announced:
        return [line(now, room_id, member) for member in members], seen, True
    output = []
    for member in members:
        ids = [ident for ident in (member.get("participant"), member.get("attempt")) if isinstance(ident, str) and ident]
        if fresh.intersection(ids):
            output.append(line(now, room_id, member, next((known[1][i] for i in ids if i in known[1]), None)))
    return output, seen, True


def run(read, follow=False, count=0, out=None, interval=None, clock=time.time, wait=time.sleep):
    """Print the snapshot (and with `follow`, every later change) to `out`; return the exit status.

    `read()` returns the panel read tuple (report, status, ..., selected room). Exit is 0 on SIGINT or a closed
    pipe; without `follow` it is the read's own status. `count` stops a follow after N collected snapshots."""
    import panel_watch
    out = out or sys.stdout
    interval = panel_watch.COLLECTION_SECONDS if interval is None else interval
    known, announced, collected = None, False, 0
    try:
        while True:
            result = read()
            report, status, selected = result[0], result[1], result[-1]
            output, known, announced = lines(report, known, selected, clock(), announced)
            for entry in output:
                out.write(entry + "\n")
            out.flush()
            collected += 1
            if not follow:
                return status
            if count and collected >= count:
                return 0
            wait(interval)
    except KeyboardInterrupt:
        return 0
    except BrokenPipeError:
        # The reader went away; point stdout at devnull so the interpreter's exit flush stays quiet.
        try:
            os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        except (OSError, ValueError):
            pass
        return 0
