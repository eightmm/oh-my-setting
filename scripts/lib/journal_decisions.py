"""Decisions as a read view of the Work Journal, and a record that a blocker was resolved.

Decisions already reach the journal as a field on ordinary events (a task
update, a park, an adopted contract); they are scattered across days. The view
collects them, newest first, from the active indexed events — no new store and
no new event kind. Resolving a blocker writes the journal's existing correction
event with `supersedes_event_id`, which is what makes the superseded event
inactive, so `show --blockers` stops listing it.
"""

import datetime as dt
import json
import re
import sqlite3

MAX_ROWS = 200
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
EVENT_ID_RE = re.compile(r"^[A-Za-z0-9._:-]{1,120}$")


def _active_events(store, since=None):
    store._ensure_index_database()
    query = "SELECT payload FROM events WHERE active = 1"
    values = ()
    if since:
        query += " AND local_date >= ?"
        values = (since,)
    query += " ORDER BY occurred_at DESC, event_id DESC"
    with sqlite3.connect(str(store.index_db_path)) as connection:
        rows = connection.execute(query, values).fetchall()
    return [store._decode_indexed_event(str(row[0])) for row in rows]


def decisions(store, since=None):
    """{"since", "decisions": [...], "omitted"}: each distinct decision text once, at its newest event."""
    if since is not None and not DATE_RE.match(since):
        raise ValueError("since")
    if since is not None:
        dt.date.fromisoformat(since)
    rows, seen = [], set()
    for event in _active_events(store, since):
        text = event.get("decision")
        if not isinstance(text, str) or not text or text in seen:
            continue
        seen.add(text)
        rows.append({
            "text": text,
            "event_id": event["event_id"],
            "event_type": event["event_type"],
            "occurred_at": event["occurred_at"],
            "local_date": event["local_date"],
            "task_id": (event.get("correlation") or {}).get("task_id") or "",
        })
    return {"since": since, "decisions": rows[:MAX_ROWS], "omitted": max(0, len(rows) - MAX_ROWS)}


def print_decisions(report, as_json):
    if as_json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
        return 0
    print("Decisions%s:" % (" (since %s)" % report["since"] if report["since"] else ""))
    if not report["decisions"]:
        print("- none recorded")
    for row in report["decisions"]:
        task = " task %s" % row["task_id"] if row["task_id"] else ""
        print("- %s (%s%s) [%s]" % (row["text"], row["local_date"], task, row["event_id"]))
    if report["omitted"]:
        print("omitted: %d older decision(s); narrow with --since YYYY-MM-DD" % report["omitted"])
    return 0


def resolve(store, event_id, how, now):
    """Record that the blocker on EVENT_ID is resolved. Returns (payload, blocker text, other open ids)."""
    if not EVENT_ID_RE.match(event_id or ""):
        raise LookupError("event id")
    how = " ".join((how or "").split())
    if not how:
        raise ValueError("how")
    target = next((event for event in _active_events(store) if event["event_id"] == event_id), None)
    if target is None:
        raise LookupError("no active event")
    blocker = target.get("blocker")
    if not isinstance(blocker, str) or not blocker:
        raise LookupError("no blocker")
    same = [event["event_id"] for event in _active_events(store)
            if event.get("blocker") == blocker and event["event_id"] != event_id]
    payload = {
        "event_type": "correction",
        "occurred_at": now,
        "source": {"type": "journal-resolve", "id": "resolve:" + event_id},
        "supersedes_event_id": event_id,
        "outcome": {"summary": "Blocker resolved: %s" % blocker[:200], "status": "resolved"},
        "decision": "Resolved blocker \"%s\": %s" % (blocker[:200], how[:400]),
        "verification_status": "not_verified",
    }
    task_id = (target.get("correlation") or {}).get("task_id")
    if task_id:
        payload["correlation"] = {"task_id": task_id}
    return payload, blocker, same
