"""Per-room read state for the Attention queue: which rows the person has already looked at.

Presentation only. Reading a row never answers, approves, dismisses or changes a call, and never hides it from
the attention-only filter. The file keeps, per room, a short hash of each read row id, the attention class word
it was read in and the time: no label, prompt, title or path. A row whose class changes is unread again."""
import hashlib
import json
import re
import time

from dashboard_projection import mapping

PATH = ".oms/artifacts/panel-attention-read.json"
LOCK = "panel-attention-read"
MAX_IDS = 100  # per room
MAX_ROOMS = 8
MAX_BYTES = 64 * 1024
LABEL = "panel attention read state"


def row_key(row_id):
    """The stored form of an attention row id."""
    return hashlib.sha256(repr(row_id).encode("utf-8")).hexdigest()[:16]


def room_key(room):
    return room if isinstance(room, str) and re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", room) else "default"


def parse(raw):
    """{room: {row key: [class word, seconds]}} from the file bytes; anything malformed is dropped."""
    try:
        data = json.loads(raw) if raw else {}
    except ValueError:
        return {}
    rooms = data.get("rooms") if isinstance(data, dict) else None
    found = {}
    for room, rows in (rooms.items() if isinstance(rooms, dict) else ()):
        if isinstance(room, str) and isinstance(rows, dict):
            found[room] = {key: value for key, value in rows.items()
                           if isinstance(key, str) and re.fullmatch(r"[0-9a-f]{16}", key) and isinstance(value, list)
                           and len(value) == 2 and isinstance(value[0], str) and re.fullmatch(r"[a-z_]{1,24}", value[0])
                           and type(value[1]) is int}
    return found


def load(repo, room):
    """{row key: class word} the person read in this room; empty when none or when the file is unreadable, so a
    failed read leaves everything unread rather than hiding a row."""
    from panel_results import _read
    try:
        rows = parse(_read(repo, PATH, MAX_BYTES, missing=True, label=LABEL)).get(room_key(room), {})
    except (OSError, ValueError):
        return {}
    return {key: value[0] for key, value in rows.items()}


def is_unread(report, item):
    """Whether an attention item is unread in this report; every item is unread when the report carries no state."""
    return mapping(report.get("attention_read")).get(row_key(item.get("row_id"))) != item.get("cls")


def mark_open(repo, room, report, row_id):
    """Mark one attention row read because the person opened it; the new room map, or None."""
    from panel_view import attention_items
    cls = next((item["cls"] for item in attention_items(report, False) if item["row_id"] == row_id), None)
    return mark(repo, room, report, [(row_id, cls)]) if cls else None


def mark(repo, room, report, entries, now=None):
    """Record `entries` (row id, class word) as read in this room and return the room's new {row key: class word}.

    Rows the report no longer holds, and rows whose class moved on, are dropped; the file stays within
    MAX_IDS per room, MAX_ROOMS rooms and MAX_BYTES. None when the report is not a completed read or the file
    could not be written (lock busy, unsafe path): nothing is then marked."""
    from panel_view import attention_items
    from panel_results import DURABLE, _lock
    if mapping(report.get("collection")).get("ok") is not True or mapping(report.get("room")).get("error"):
        return None
    current = {row_key(item["row_id"]): item["cls"] for item in attention_items(report, False)}
    seconds, name, saved = int(time.time() if now is None else now), room_key(room), {}

    def transform(raw):
        rooms = parse(raw)
        rows = {key: value for key, value in rooms.get(name, {}).items() if current.get(key) == value[0]}
        for row_id, cls in entries:
            if current.get(row_key(row_id)) == cls:
                rows[row_key(row_id)] = [cls, seconds]
        rows = dict(sorted(rows.items(), key=lambda entry: (-entry[1][1], entry[0]))[:MAX_IDS])
        rooms.pop(name, None)
        if rows:
            rooms[name] = rows
        newest = sorted(rooms, key=lambda each: max(value[1] for value in rooms[each].values()), reverse=True)
        saved.update(rows)
        return (json.dumps({"schema": 1, "rooms": {each: rooms[each] for each in newest[:MAX_ROOMS]}},
                           sort_keys=True) + "\n").encode()
    try:
        with _lock(repo, LOCK, timeout=.1):
            path = DURABLE["canonical_repo_path"](str(repo), PATH, LABEL, True)
            DURABLE["atomic_mutate"](path, transform, str(repo), missing_ok=True, create_parent=True,
                                     max_output=MAX_BYTES, label=LABEL)
    except (OSError, ValueError, SystemExit):
        return None
    return {key: value[0] for key, value in saved.items()}
