"""Editable intent followed by one explicit, reconcilable OMS room delivery."""
from __future__ import annotations

from contextlib import nullcontext
import os
from pathlib import Path
import subprocess

import room
import thread_live
from oms_runtime.common import CoreError, canonical_json, sha256_bytes, utc_now
from . import attachments
from .common import context, ident, item, locked, new_id, plain, safe_text, state, write_state


def history(repo, room_id):
    selected = room.resolve(repo, ident(room_id), fresh=True)
    names = list(reversed(list(room._sealed_predecessors(repo, selected)))) + [selected]
    result, total = [], 0
    for name in names:
        path = plain(thread_live.thread_path(repo, name))
        total += path.stat().st_size
        if total > 32 * 1024 * 1024:
            raise CoreError("room lineage exceeds the 32 MiB reconciliation bound")
        result.append((name, room.records(repo, name)))
    return result


def _ordinary_rows(histories):
    """Carried copies preserve evidence but cannot mint a new enrollment epoch."""
    for name, rows in histories:
        carried = bool(len(rows) > 1 and rows[1].get("room_event", {}).get("predecessor"))
        for row in rows:
            event = row.get("room_event", {})
            if event.get("kind") == "carried":
                carried = False
                continue
            if not carried:
                yield name, row


def _identity(histories, who, to):
    current = room.project(histories[-1][1])
    if current["closed"] or current["successor"] or current["predecessor"] and not current["carried"]:
        raise CoreError("input target room is closed, sealed, or incompletely carried")
    if who == to or to == "all":
        raise CoreError("inputs require distinct explicit sender and recipient participants")
    sender, recipient = room.participant(current, ident(who)), room.participant(current, ident(to))
    owner = room.participant(current, current["owner"])
    epochs = {}
    for name, row in _ordinary_rows(histories):
        event = row.get("room_event", {})
        if event.get("kind") in {"join", "leave", "bind", "main-swap", "scope"}:
            epochs[event.get("participant")] = {"room": name, "seq": row["seq"]}
    def signature(member):
        result = {key: member.get(key) for key in ("participant", "provider", "role", "consumer",
                                                  "parent", "task_id", "owns")}
        result["epoch"] = epochs.get(member["participant"])
        if result["epoch"] is None:
            raise CoreError("participant enrollment provenance is unavailable")
        return result
    return {"root_room": histories[0][0], "task_id": current["task_id"],
            "sender": signature(sender), "recipient": signature(recipient), "owner": signature(owner)}


def binding(repo, room_id, who, to):
    histories = history(repo, room_id)
    return {"room": ident(room_id), "resolved_room": histories[-1][0], "from": ident(who), "to": ident(to),
            "identity": _identity(histories, who, to)}


def attachment_refs(repo, keys):
    if len(keys) > 8 or len(set(keys)) != len(keys):
        raise CoreError("an input accepts at most eight distinct attachments")
    result = []
    for key in keys:
        record, _ = attachments.load(repo, key)
        attachments.validate(repo, record)
        result.append({"id": record["id"], "content_sha256": record["content_sha256"],
                       "record_sha256": sha256_bytes(canonical_json(record)), "kind": record["kind"],
                       "source_verified": bool(record["source"].get("verified"))})
    return result


def payload(row):
    text = "OMS queued input / %s / %s\n\n%s" % (row["mode"], row["id"], row["text"].rstrip("\r\n"))
    for ref in row["attachments"]:
        text += "\n\nAttachment %s (%s; %s)\nsha256 %s\nRead: oms input attachment %s" % (
            ref["id"], ref["kind"], "source verified at capture" if ref["source_verified"] else "declared origin, unverified",
            ref["content_sha256"], ref["id"])
    text += "\n\nDelivery records a room message; it does not prove execution or approval."
    safe_text(text, 4000)
    return text


def make_item(repo, work, route, text, mode, keys, provenance=None):
    if mode not in {"steer", "next"}:
        raise CoreError("mode must be steer or next")
    row = {"id": new_id("input"), "created_at": utc_now(), "version": 1, "status": "queued",
           "worktree": str(work), "binding": route, "mode": mode, "text": safe_text(text, 3500),
           "attachments": attachment_refs(repo, keys)}
    row["message_id"] = "msg-" + row["id"]
    if provenance is not None:
        row["provenance"] = provenance
    payload(row)
    return row


def add(repo, work, room_id, who, to, text, mode="next", keys=()):
    route = binding(repo, room_id, who, to)
    row = make_item(repo, work, route, text, mode, keys)
    with locked(repo):
        value = state(repo)
        if len(value["items"]) >= 256:
            raise CoreError("input queue has reached its 256-item bound")
        value["items"].append(row)
        write_state(repo, value)
    return row


def edit(repo, key, text=None, mode=None, keys=None):
    with locked(repo):
        value = state(repo)
        row = item(value, key)
        if row["status"] != "queued":
            raise CoreError("only queued inputs may be edited")
        if text is not None:
            row["text"] = safe_text(text, 3500)
        if mode is not None:
            row["mode"] = mode
        if keys is not None:
            row["attachments"] = attachment_refs(repo, keys)
        payload(row)
        row["version"] += 1
        row["edited_at"] = utc_now()
        write_state(repo, value)
        return row


def move(repo, key, before=None):
    with locked(repo):
        value = state(repo)
        row = item(value, key)
        if row["status"] != "queued":
            raise CoreError("only queued inputs may be reordered")
        if before == key:
            return row
        if before and item(value, before)["status"] != "queued":
            raise CoreError("move target must be queued")
        value["items"].remove(row)
        index = next((n for n, found in enumerate(value["items"]) if found["id"] == before), len(value["items"]))
        value["items"].insert(index, row)
        write_state(repo, value)
        return row


def cancel(repo, key):
    with locked(repo):
        value = state(repo)
        row = item(value, key)
        if row["status"] != "queued":
            raise CoreError("only queued inputs may be cancelled; reconcile an uncertain delivery")
        row.update(status="cancelled", cancelled_at=utc_now())
        write_state(repo, value)
        return row


def preview(repo, row):
    route = row["binding"]
    work, shared = context(row["worktree"])
    if str(work) != row["worktree"] or shared != repo:
        raise CoreError("input source worktree or shared repository ownership changed")
    current = binding(repo, route["room"], route["from"], route["to"])
    if current["identity"] != route["identity"]:
        raise CoreError("input sender, recipient, or room owner changed; create a new explicitly addressed input")
    refs = attachment_refs(repo, [ref["id"] for ref in row["attachments"]])
    if refs != row["attachments"]:
        raise CoreError("immutable attachment metadata or content changed")
    text = payload(row)
    return {"id": row["id"], "mode": row["mode"], "version": row["version"], "message_id": row["message_id"],
            "room": current["resolved_room"], "from": route["from"], "to": route["to"],
            "text": text, "payload_sha256": sha256_bytes(text.encode()), "bytes": len(text.encode()),
            "attachments": refs, "at_boundary_required": row["mode"] == "next", "executes_model": False}


def _receipt(repo, row):
    intent = row.get("delivery")
    if not intent:
        raise CoreError("input has no delivery intent to reconcile")
    if (intent.get("message_id") != row["message_id"] or intent.get("text") != payload(row)
            or intent.get("payload_sha256") != sha256_bytes(intent["text"].encode())):
        raise CoreError("delivery intent no longer matches the immutable queued payload")
    histories = history(repo, row["binding"]["room"])
    matches = []
    for index, (name, rows) in enumerate(histories):
        ordinary = {(n, r["seq"]) for n, r in _ordinary_rows([(name, rows)])}
        for offset, event_row in enumerate(rows):
            event = event_row.get("room_event", {})
            if event.get("kind") != "message" or event.get("id") != row["message_id"]:
                continue
            if (event.get("sender") != intent["from"] or event.get("recipient") != intent["to"]
                    or event.get("message_kind") != "note" or event_row.get("text") != intent["text"]):
                raise CoreError("message ID exists with different content or addressing; delivery remains uncertain")
            if (name, event_row["seq"]) not in ordinary:
                continue
            historical = histories[:index] + [(name, rows[:offset + 1])]
            if _identity(historical, intent["from"], intent["to"]) != row["binding"]["identity"]:
                raise CoreError("message was appended under a different participant or room-owner identity")
            matches.append({"room": name, "seq": event_row["seq"], "ts": event_row.get("ts")})
    if len(matches) > 1:
        raise CoreError("message ID has multiple original room deliveries")
    if not matches:
        return None
    acknowledgments = [{"room": name, "seq": event_row["seq"]} for name, event_row in _ordinary_rows(histories)
                       if event_row.get("room_event", {}).get("kind") == "received"
                       and event_row["room_event"].get("participant") == intent["to"]
                       and row["message_id"] in event_row["room_event"].get("messages", [])]
    return dict(matches[0], message_id=row["message_id"], payload_sha256=intent["payload_sha256"],
                acknowledged=bool(acknowledgments), acknowledgments=acknowledgments,
                authority="persisted room message; acknowledgment is not approval or execution proof")


def authorize_child_attachment(repo, key):
    """A shared checkout is not a grant to read another task's selected input."""
    inherited_repo = os.environ.get("OMS_ROOM_REPO")
    selected_room = os.environ.get("OMS_ROOM_ID")
    who = os.environ.get("OMS_ROOM_PARTICIPANT")
    if not inherited_repo or not selected_room or not who or Path(inherited_repo).resolve() != repo:
        raise CoreError("worker attachment reads require an admitted room repository and participant")
    member = room.participant(room.project(history(repo, selected_room)[-1][1]), who)
    if member.get("role") == "main" or not member.get("parent"):
        raise CoreError("worker attachment reads require an admitted child participant")
    record, _ = attachments.load(repo, ident(key))
    record_sha = sha256_bytes(canonical_json(record))
    for row in state(repo)["items"]:
        route = row["binding"]
        if (row["status"] != "delivered" or route["to"] != who
                or not room.same_room(repo, route["room"], selected_room)):
            continue
        ref = next((ref for ref in row["attachments"] if ref["id"] == key), None)
        if ref is None or ref["record_sha256"] != record_sha or ref["content_sha256"] != record["content_sha256"]:
            continue
        current = binding(repo, route["room"], route["from"], route["to"])
        if current["identity"] == route["identity"] and _receipt(repo, row) is not None:
            return
    raise CoreError("attachment was not confirmed delivered to this admitted worker identity")


def deliver(repo, key, apply=False, at_boundary=False, expected=None):
    with locked(repo) if apply else nullcontext():
        value = state(repo)
        row = item(value, key)
        if row["status"] == "delivered":
            return dict(row, deduplicated=True)
        if row["status"] != "queued":
            raise CoreError("input is not queued; reconcile uncertainty before any further delivery")
        plan = preview(repo, row)
        if expected is not None and plan["payload_sha256"] != expected:
            raise CoreError("input changed since preview; inspect its current payload digest")
        if not apply:
            return dict(plan, dry_run=True)
        if row["mode"] == "next" and not at_boundary:
            raise CoreError("next-task delivery requires an explicit --at-boundary acknowledgment")
        lane = lambda other: (other["binding"]["identity"]["root_room"], other["binding"]["from"],
                              other["binding"]["to"], other["mode"])
        first = next(other for other in value["items"] if other["status"] == "queued" and lane(other) == lane(row))
        if first["id"] != key:
            raise CoreError("an earlier input is queued for this target and mode; reorder it explicitly")
        row["delivery"] = dict(plan, intent_at=utc_now())
        row["status"] = "uncertain"
        write_state(repo, value)
        try:
            room.send(repo, plan["room"], plan["from"], plan["to"], plan["text"], "note",
                      message_id=plan["message_id"], linked=False)
            receipt = _receipt(repo, row)
            if receipt is not None:
                row.update(status="delivered", receipt=receipt)
        except (OSError, ValueError, CoreError, subprocess.SubprocessError):
            row["delivery_error"] = "room append failed or its exact result could not be verified"
        write_state(repo, value)
        return row


def reconcile(repo, key):
    with locked(repo):
        value = state(repo)
        row = item(value, key)
        if row["status"] not in {"uncertain", "delivered"}:
            raise CoreError("only attempted deliveries can be reconciled")
        receipt = _receipt(repo, row)
        if receipt is not None:
            row.update(status="delivered", receipt=receipt)
            row.pop("delivery_error", None)
        row["last_reconciled_at"] = utc_now()
        write_state(repo, value)
        return dict(row, observed=receipt is not None, retry_allowed=False)
