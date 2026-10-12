"""Task-scoped participation and messages over the canonical OMS thread log."""

import argparse
import base64
import calendar
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

import thread_live
from dashboard_projection import clean
from path_scope import normalize as normalize_scope
from room_repository import state_repository

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / "scripts" / "oms"
ROLES = {"main", "worker", "advisor", "reviewer", "researcher"}
KINDS = {"question", "answer", "note", "decision", "handoff", "status"}
MAX_PARTICIPANTS = 32
# Call slots stay bounded even when a full mailbox cannot deliver notifications.
MAX_CALLS = 480
MAX_MESSAGES = 1024
MAIN_RESERVE = 8
CALL_RESERVE = 64
MESSAGE_RESERVE = 128
MAX_ROOMS = 64
MAX_SCAN_ENTRIES = 1024
MAX_SCAN_BYTES = 8 * 1024 * 1024
# A room past these marks rolls over to a linked successor at the next dispatch;
# the reserves above stay for the carried state and in-flight writers.
ROLLOVER_MESSAGES = MAX_MESSAGES - MESSAGE_RESERVE
ROLLOVER_CALLS = MAX_CALLS - CALL_RESERVE
# Essential carried state may exceed the reserves but must leave its successor half the way to the marks.
CARRY_MESSAGES = ROLLOVER_MESSAGES // 2
CARRY_CALLS = ROLLOVER_CALLS // 2
MAX_HOPS = 64
TAIL_BYTES = 8192
RESOLVE_TTL = 2
_RESOLVED, _RECENT = {}, {}
_CLOCK = time.monotonic  # bound at import: board fixtures script time.monotonic for their own loops
# Seconds an unanswered question must stay open before each escalation tier.
ACK_AFTER = REPEAT_AFTER = 300
ASKER_AFTER, OVERDUE_AFTER, PERSON_AFTER, SECOND_REMINDER_AFTER = 600, 900, 1200, 1800
MAX_REMINDERS = 3


def identifier(value, label="identifier"):
    if not isinstance(value, str) or not thread_live.ID.fullmatch(value) or value.startswith(("-", ".")):
        raise ValueError("invalid " + label)
    return value


def records(repo, room):
    with thread_live.open_thread(repo, room) as handle:
        data = handle.read(thread_live.MAX_FILE + 1)
    return decode_records(data, room)


def decode_records(data, room):
    if len(data) > thread_live.MAX_FILE or (data and not data.endswith(b"\n")):
        raise ValueError("room log is oversized or has an incomplete turn")
    if any(len(line) > thread_live.MAX_ROW for line in data.splitlines()):
        raise ValueError("oversized room row")
    rows = [json.loads(line) for line in data.splitlines() if line.strip()]
    if any(not isinstance(row, dict) or row.get("thread") != room or not isinstance(row.get("seq"), int)
           or not isinstance(row.get("room_event", {}), dict) for row in rows):
        raise ValueError("invalid room log")
    return rows


def discover(repo):
    """Bound reads, and never infer unique enrollment from incomplete evidence."""
    found, scanned, incomplete = [], 0, False
    folder = Path(repo) / ".oms/threads"
    try:
        with os.scandir(folder) as entries:
            for index, entry in enumerate(entries):
                if index >= MAX_SCAN_ENTRIES:
                    incomplete = True
                    break
                if not entry.name.endswith(".jsonl"):
                    continue
                try:
                    with thread_live.open_thread(repo, Path(entry.name).stem) as handle:
                        prefix = []
                        for _ in range(2):
                            line = handle.readline(min(thread_live.MAX_ROW + 1, MAX_SCAN_BYTES - scanned + 1))
                            scanned += len(line)
                            if scanned > MAX_SCAN_BYTES or len(line) > thread_live.MAX_ROW:
                                raise ValueError("room discovery byte limit")
                            prefix.append(line)
                        header = json.loads(prefix[0])
                        if not isinstance(header, dict) or header.get("thread") != Path(entry.name).stem:
                            raise ValueError("invalid discovery header")
                        second = json.loads(prefix[1]) if prefix[1] else {}
                        if not isinstance(second, dict) or not isinstance(second.get("room_event", {}), dict):
                            raise ValueError("invalid discovery event")
                        event = second.get("room_event", {})
                        if not event:
                            continue
                        if event.get("kind") != "created":
                            raise ValueError("missing room creation event")
                        successor = _tail_successor(repo, Path(entry.name).stem)
                        if successor:
                            # A sealed log is immutable history: its header and last row describe it.
                            found.append(dict(_STUB, id=header["thread"], title=second.get("text", "Shared work"),
                                              task_id=event.get("task_id"), creator=event.get("participant"),
                                              predecessor=event.get("predecessor"), successor=successor))
                            continue
                        if len(found) - sum(bool(s.get("sealed")) for s in found) >= MAX_ROOMS:
                            incomplete = True
                            break
                        suffix = handle.read(MAX_SCAN_BYTES - scanned + 1)
                        scanned += len(suffix)
                        if scanned > MAX_SCAN_BYTES:
                            raise ValueError("room discovery byte limit")
                        state = project(decode_records(b"".join(prefix) + suffix, Path(entry.name).stem))
                        if (state["predecessor"] and not state["carried"]
                                and _tail_successor(repo, state["predecessor"]) != state["id"]):
                            continue  # still being carried before its seal, or left by a rollover that never sealed
                        found.append(state)
                except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError):
                    incomplete = True
                    if scanned >= MAX_SCAN_BYTES:
                        break
    except FileNotFoundError:
        pass
    except OSError:
        incomplete = True
    return sorted(found, key=lambda state: state["id"]), incomplete


_STUB = {"schema": 1, "kind": "oms-room", "owner": None, "closed": False, "sealed": True, "participants": [],
         "messages": [], "publication": None, "publications": {},
         "delivery": "next safe point or explicit poll; acknowledgment is not approval"}


def successor_id(old):
    return "room-" + hashlib.sha256(("successor:" + old).encode("utf-8")).hexdigest()[:12]


def _tail_successor(repo, ident):
    """The successor named by a sealed log's last row, read from at most TAIL_BYTES; None otherwise."""
    try:
        with thread_live.open_thread(repo, ident) as handle:
            size = os.fstat(handle.fileno()).st_size
            if not size:
                return None
            offset = max(0, size - TAIL_BYTES)
            handle.seek(offset)
            chunk = handle.read(TAIL_BYTES)
    except (OSError, ValueError):
        return None
    if not chunk.endswith(b"\n"):
        return None
    start = chunk.rfind(b"\n", 0, len(chunk) - 1)
    if start < 0 and offset:
        return None
    try:
        row = json.loads(chunk[start + 1:])
    except (ValueError, RecursionError):
        return None
    event = row.get("room_event") if isinstance(row, dict) else None
    if (not isinstance(event, dict) or row.get("thread") != ident or event.get("kind") != "successor"
            or not isinstance(event.get("target"), str) or not thread_live.ID.fullmatch(event["target"])):
        return None
    return event["target"]


def _header(repo, ident):
    with thread_live.open_thread(repo, ident) as handle:
        lines = [handle.readline(thread_live.MAX_ROW + 1) for _ in range(2)]
    if any(len(line) > thread_live.MAX_ROW or line and not line.endswith(b"\n") for line in lines):
        raise ValueError("room header is oversized or incomplete")
    return decode_records(b"".join(lines), ident)


def _predecessor(repo, ident):
    try:
        rows = _header(repo, ident)
    except (OSError, ValueError, TypeError):
        return None
    event = rows[1].get("room_event", {}) if len(rows) == 2 else {}
    return event.get("predecessor") if event.get("kind") == "created" else None


def resolve(repo, ident, fresh=False):
    """The room an id now names: follow sealed logs to the successor that names them back; fail closed.

    Board loops compare many attempt refs per tick, so answers are reused for RESOLVE_TTL seconds; a stale
    answer only sends a write to the sealed log, which refuses it and makes append() resolve fresh."""
    if not isinstance(ident, str) or not thread_live.ID.fullmatch(ident):
        return ident
    recent = _RECENT.get((str(repo), ident))
    if not fresh and recent and 0 <= _CLOCK() - recent[0] < RESOLVE_TTL:
        return recent[1]
    found = _resolve(repo, ident)
    _RECENT[(str(repo), ident)] = (_CLOCK(), found)
    return found


def _resolve(repo, ident):
    cur, seen = ident, {ident}
    for _ in range(MAX_HOPS):
        try:
            info = thread_live.thread_path(repo, cur).stat()
            stamp = (info.st_dev, info.st_ino, info.st_size)
        except (OSError, ValueError):
            return cur
        cached = _RESOLVED.get((str(repo), cur))
        if cached and cached[0] == stamp:
            nxt = cached[1]
        else:
            nxt = _tail_successor(repo, cur)
            # A successor that does not name this log back is not followed; writes then meet the seal.
            if nxt and (nxt in seen or _predecessor(repo, nxt) != cur):
                nxt = None
            _RESOLVED[(str(repo), cur)] = (stamp, nxt)
        if not nxt or nxt in seen:
            return cur
        seen.add(nxt)
        cur = nxt
    raise ValueError("room forwarding chain too long")


def same_room(repo, a, b):
    return a == b or bool(a and b and resolve(repo, a) == resolve(repo, b))


def _bound_room(rows, repo):
    """Whether this session's room binding names the room being written, directly or through a rollover."""
    bound = os.environ.get("OMS_ROOM_ID")
    if not bound or not rows:
        return False
    if bound == rows[0].get("thread"):
        return True
    where = os.environ.get("OMS_ROOM_REPO") or repo
    return bool(where) and resolve(where, bound) == rows[0].get("thread")


def _child():
    return os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"


def _carrying():
    """Whether this write replays a sealed room's state into its successor rather than speaking for a session."""
    return os.environ.get("OMS_ROOM_CARRY") == "1"


def event_shape(event):
    if not isinstance(event, dict):
        raise ValueError("invalid persisted room event")
    kind = event.get("kind")
    required = {"created": (), "join": ("participant", "provider", "role", "label"), "bind": ("participant", "consumer"),
                "scope": ("participant",), "describe": ("participant", "label"),
                "leave": ("participant",), "closed": ("participant",), "message": ("id", "sender", "recipient", "message_kind"),
                "received": ("participant",), "publication": ("id", "digest", "status"),
                "successor": ("participant", "target"), "carried": ()}
    if not event:
        return
    if kind not in required or any(not isinstance(event.get(key), str) for key in required[kind]):
        raise ValueError("invalid persisted room event")
    for key in ("model", "consumer", "previous", "parent", "reply_to", "task_id", "turn_id", "target", "participant",
                "predecessor"):
        if event.get(key) is not None and not isinstance(event[key], str):
            raise ValueError("invalid room event field")
    for key in ("owns", "messages", "audience"):
        if key in event and (not isinstance(event[key], list) or any(not isinstance(value, str) for value in event[key])):
            raise ValueError("invalid room event list")
    if kind == "scope" and "owns" not in event:
        raise ValueError("missing declared scopes")
    if kind == "received" and "messages" not in event:
        raise ValueError("missing room receipt ids")
    if kind == "publication" and type(event.get("delivery_unknown")) is not bool:
        raise ValueError("invalid room delivery receipt")
    if kind == "publication" and "target" in event and not isinstance(event["target"], str):
        raise ValueError("invalid room delivery target")
    if kind == "describe":
        if set(event) - {"kind", "participant", "label"}:
            raise ValueError("describe changes only the participant label")
        if not event["label"] or event["label"] != clean(event["label"], 120):
            raise ValueError("use a bounded plain participant label")


def project(rows):
    for row in rows:
        event_shape(row.get("room_event", {}))
        if not isinstance(row.get("text", ""), str):
            raise ValueError("invalid room text")
    created = next((row for row in rows if row.get("room_event", {}).get("kind") == "created"), None)
    if created is None:
        raise ValueError("this thread is not a work room")
    members, messages, receipts, owner, publication, publications, successor = {}, [], {}, None, None, {}, None
    carried = False
    for row in rows:
        event = row.get("room_event", {})
        kind = event.get("kind")
        if kind == "join":
            previous = members.get(event["participant"])
            original = previous.get("initial_consumer") if previous is not None else event.get("consumer")
            initial_seq = previous["initial_seq"] if previous is not None else row["seq"]
            members[event["participant"]] = dict(event, joined=True, seq=row["seq"], joined_at=row.get("ts", ""))
            members[event["participant"]]["initial_consumer"] = original
            members[event["participant"]]["initial_seq"] = initial_seq
            if owner is None and event["role"] == "main":
                owner = event["participant"]
        elif kind == "leave" and event["participant"] in members:
            members[event["participant"]]["joined"] = False
        elif kind == "describe":
            member = members.get(event["participant"])
            if not member or not member["joined"] or member["role"] != "main":
                raise ValueError("describe requires an enrolled main")
            member["label"] = event["label"]
            member["label_override"] = event["label"]
        elif kind == "bind":
            if event["participant"] not in members:
                raise ValueError("native binding has no participant")
            members[event["participant"]]["consumer"] = event["consumer"]
            if (not members[event["participant"]].get("initial_consumer")
                    and members[event["participant"]]["seq"] == members[event["participant"]]["initial_seq"]):
                members[event["participant"]]["initial_consumer"] = event["consumer"]
        elif kind == "scope":
            if event["participant"] not in members:
                raise ValueError("declared scope has no participant")
            members[event["participant"]]["owns"] = list(event["owns"])
        elif kind == "message":
            # A carried broadcast keeps the audience it had when it was sent.
            targets = (list(event["audience"]) if "audience" in event
                       else [p for p, member in members.items() if member["joined"] and p != event["sender"]]
                       if event["recipient"] == "all" else [event["recipient"]])
            if event["message_kind"] == "status":
                targets = []
            messages.append(dict(event, targets=targets, text=row.get("text", ""), seq=row["seq"], ts=row.get("ts")))
        elif kind == "publication":
            publication = dict(event, seq=row["seq"])
            publications[event.get("target", "codex")] = publication
        elif kind == "received":
            for mid in event["messages"]:
                receipts.setdefault(mid, set()).add(event["participant"])
        elif kind == "successor":
            # Sealing is not closing: members stay joined, so proofs read against this log still hold.
            successor = event["target"]
        elif kind == "carried":
            carried = True
    answered = {m.get("reply_to") for m in messages if m["message_kind"] == "answer"}
    for message in messages:
        targets = message["targets"]
        message["received_by"] = sorted(receipts.get(message["id"], set()))
        message["pending_for"] = [p for p in targets if p not in receipts.get(message["id"], set())]
        message["answered"] = message["id"] in answered
    return {"schema": 1, "kind": "oms-room", "id": created["thread"],
            "title": created.get("text", "Shared work"), "owner": owner,
            "task_id": created["room_event"].get("task_id"),
            "creator": created["room_event"].get("participant"),
            "closed": any(row.get("role") == "closed" or row.get("room_event", {}).get("kind") == "closed" for row in rows),
            "participants": list(members.values()), "messages": messages, "publication": publication,
            "publications": publications, "successor": successor,
            "predecessor": created["room_event"].get("predecessor"), "carried": carried,
            "delivery": "next safe point or explicit poll; acknowledgment is not approval"}


def participant(state, value):
    identifier(value, "participant")
    found = next((p for p in state["participants"] if p["participant"] == value and p["joined"]), None)
    if found is None:
        raise ValueError("participant has not joined this room")
    return found


def message_capacity_reached(state):
    return len(state["messages"]) >= MAX_MESSAGES


def overlaps(a, b):
    """Declared scopes overlap when equal or one is a path-component prefix of the other; globs compare literally."""
    return a == b or "." in (a, b) or b.startswith(a + "/") or a.startswith(b + "/")


def declared_scopes(role, scopes):
    if not isinstance(scopes, list) or len(scopes) > 16:
        raise ValueError("at most sixteen declared scopes")
    if role == "researcher" and scopes:
        raise ValueError("researchers own no write scope")
    if role in {"advisor", "reviewer"} and scopes:
        raise ValueError("advisors and reviewers own no write scope")
    for scope in scopes:
        if normalize_scope(scope) != scope:
            raise ValueError("use normalized repository-relative scopes")


def validate_event(rows, event, text, repo=None):
    """Validate while thread.sh holds its existing file lock; no membership grants authority."""
    if not isinstance(event, dict) or event.get("kind") not in {"created", "join", "bind", "scope", "describe", "leave", "closed", "message", "received", "publication", "successor", "carried"}:
        raise ValueError("invalid room event")
    event_shape(event)
    kind = event["kind"]
    carry = _carrying()
    keys = {"created": {"kind", "task_id", "participant", "predecessor"},
            "join": {"kind", "participant", "provider", "model", "role", "label", "consumer", "owns", "parent"},
            "bind": {"kind", "participant", "consumer", "previous"},
            "scope": {"kind", "participant", "owns"},
            "describe": {"kind", "participant", "label"},
            "leave": {"kind", "participant"},
            "closed": {"kind", "participant"},
            "message": {"kind", "id", "sender", "recipient", "message_kind", "reply_to"} | ({"audience"} if carry else set()),
            "received": {"kind", "participant", "messages"},
            "publication": {"kind", "id", "digest", "status", "delivery_unknown", "turn_id", "retry", "target"},
            "successor": {"kind", "participant", "target"}, "carried": {"kind"}}[kind]
    if set(event) - keys:
        raise ValueError("unknown room event fields")
    metadata = json.dumps(event, ensure_ascii=False)
    if len(metadata.encode("utf-8")) > 8192:
        raise ValueError("room metadata is oversized")
    check = subprocess.run(["bash", "-c",
        '. "$1"; grep -Eiq "$(agent_memory_sensitive_re)"', "room-metadata",
        str(ROOT / "scripts/lib/agent-memory-common.sh")], input=metadata,
        capture_output=True, text=True, timeout=5)
    if check.returncode != 1:
        raise ValueError("room metadata contains sensitive-looking content or could not be checked")
    child = _child()
    if child and carry:
        raise ValueError("a worker cannot carry a room")
    if child and (not rows or not _bound_room(rows, repo)
                  or kind not in {"message", "received"} or
                  event.get("sender", event.get("participant")) != os.environ.get("OMS_ROOM_PARTICIPANT")):
        raise ValueError("a worker can only message or acknowledge as its admitted room participant")
    if kind == "created":
        if len(rows) != 1 or rows[0].get("live") is not True or any(row.get("room_event") for row in rows):
            raise ValueError("room already exists")
        if event.get("participant"):
            identifier(event["participant"], "creator")
        if event.get("task_id"):
            identifier(event["task_id"], "task")
        if event.get("predecessor") is not None and (
                identifier(event["predecessor"], "predecessor room") == rows[0].get("thread")):
            raise ValueError("a room cannot succeed itself")
        return True
    state = project(rows)
    if state["successor"]:
        if kind == "successor" and event["target"] == state["successor"]:
            return False
        raise ValueError("room continues as %s" % state["successor"])
    if state["predecessor"]:
        # Until its carry completes a successor holds only carried state, so a rerun can compare it with the
        # sealed log; anything else waits for the carry, which the writer's retry finishes.
        if carry and state["carried"]:
            if kind == "carried":
                return False
            raise ValueError("room carry is complete")
        if not carry and not state["carried"]:
            raise ValueError("room carry is unfinished")
    elif carry or kind == "carried":
        raise ValueError("only a successor room takes carried state")
    if kind == "carried":
        if repo is not None and _tail_successor(repo, state["predecessor"]) != rows[0]["thread"]:
            raise ValueError("a carry completes only after its predecessor is sealed")
        return True
    if child:
        member = participant(state, os.environ.get("OMS_ROOM_PARTICIPANT"))
        if member["role"] == "main":
            raise ValueError("a worker cannot speak as a native main")
        if kind == "message":
            if str(event["id"]).startswith("result-"):
                raise ValueError("result handoffs are written by the panel, not by a worker")
            parent = member.get("parent")
            if not parent or event.get("recipient") != parent:
                raise ValueError("a worker can only message its parent %s" % (parent or "<missing>"))
    if kind == "closed":
        who = identifier(event["participant"], "participant")
        caller = os.environ.get("OMS_ROOM_PARTICIPANT")
        if caller and (caller != who or not _bound_room(rows, repo)):
            raise ValueError("close must name this session's participant and room")
        if who != state["creator"]:
            participant(state, who)
        return not state["closed"]
    if state["closed"] and kind != "received":
        raise ValueError("room is closed")
    if kind == "successor":
        member = participant(state, event["participant"])
        if member["role"] != "main" or member.get("parent"):
            raise ValueError("a rollover is recorded by an enrolled main")
        if event["target"] != successor_id(rows[0]["thread"]):
            raise ValueError("successor room id does not derive from this room")
        if repo is not None and _predecessor(repo, event["target"]) != rows[0]["thread"]:
            raise ValueError("successor room does not name this room as its predecessor")
    elif kind == "describe":
        member = participant(state, event["participant"])
        if member["role"] != "main":
            raise ValueError("describe requires an enrolled main")
        bound_room = os.environ.get("OMS_ROOM_ID")
        bound_participant = os.environ.get("OMS_ROOM_PARTICIPANT")
        if ((bound_room or bound_participant)
                and (not _bound_room(rows, repo) or bound_participant != event["participant"])):
            raise ValueError("describe must name this session's main and room")
        if member.get("label_override") == event["label"]:
            return False
    elif kind == "bind":
        member = participant(state, event.get("participant"))
        consumer = event.get("consumer")
        if member["role"] != "main" or member["provider"] not in {"codex", "claude"}:
            raise ValueError("native binding is for an enrolled Codex/Claude main")
        if not isinstance(consumer, str) or not re.fullmatch(r"[0-9a-f]{32}", consumer):
            raise ValueError("invalid native session binding")
        if member.get("consumer"):
            if member["consumer"] == consumer:
                return False
            # Only a rebind naming the identity it replaces moves a binding; a stale or blind one is refused.
            if event.get("previous") != member["consumer"]:
                raise ValueError("native identity is already bound; leave and rejoin before changing it")
        if any(p.get("consumer") == consumer and p["joined"] for p in state["participants"]):
            raise ValueError("native session already has a participant in this room")
    elif kind == "scope":
        # Harness children never reach here (message/ack only); an inherited caller identity must match.
        caller = os.environ.get("OMS_ROOM_PARTICIPANT")
        if caller and caller != event["participant"]:
            raise ValueError("only a participant can declare its own scope")
        member = participant(state, event["participant"])
        declared_scopes(member["role"], event["owns"])
        if member.get("owns", []) == event["owns"]:
            return False
    elif kind == "publication":
        identifier(event.get("id"), "publication")
        identifier(event.get("status"), "delivery status")
        if not isinstance(event.get("digest"), str) or not re.fullmatch(r"[0-9a-f]{64}", event["digest"]):
            raise ValueError("invalid publication digest")
        if type(event.get("delivery_unknown")) is not bool or type(event.get("retry", False)) is not bool:
            raise ValueError("invalid publication receipt")
        if event.get("turn_id"):
            identifier(event["turn_id"], "app turn")
        target = event.get("target", "codex")
        if target != "codex":
            if not target.startswith("claude."):
                raise ValueError("invalid app delivery target")
            member = participant(state, target[7:])
            if member["provider"] != "claude" or member["role"] != "main" or not member.get("consumer"):
                raise ValueError("Claude app delivery needs an enrolled native main")
        prior = state["publications"].get(target)
        if event["status"] == "pending":
            if prior and not event.get("retry") and (prior["status"] == "pending" or prior["delivery_unknown"]
                    or prior["digest"] == event["digest"] and prior["status"] == "persisted"):
                raise ValueError("snapshot already submitted; inspect its receipt before explicit retry")
        elif (not prior or prior["id"] != event["id"] or prior["digest"] != event["digest"]
              or prior["status"] != "pending"):
            raise ValueError("publication receipt does not match its durable intent")
    elif kind == "join":
        identifier(event.get("participant"), "participant")
        identifier(event.get("provider"), "provider")
        check = subprocess.run(["bash", "-c", '. "$1"; oms_provider_normalize "$2"', "room-provider",
                                str(ROOT / "scripts/lib/provider-registry.sh"), event["provider"]],
                               capture_output=True, text=True, timeout=5)
        if check.returncode or check.stdout.strip() != event["provider"]:
            raise ValueError("use a canonical registered provider")
        if event.get("role") not in ROLES:
            raise ValueError("invalid room role")
        model = event.get("model")
        if model is not None and (not isinstance(model, str) or len(model) > 160
                                  or not re.fullmatch(r"[A-Za-z0-9_./:-]+", model)):
            raise ValueError("invalid room model")
        label = event.get("label")
        if not isinstance(label, str) or not label or label != clean(label, 120):
            raise ValueError("use a bounded plain participant label")
        consumer = event.get("consumer")
        if consumer is not None and (not isinstance(consumer, str) or not re.fullmatch(r"[0-9a-f]{32}", consumer)):
            raise ValueError("invalid native session binding")
        if consumer and any(p.get("consumer") == consumer and p["joined"]
                            and p["participant"] != event["participant"] for p in state["participants"]):
            raise ValueError("native session already has a participant in this room")
        declared_scopes(event["role"], event.get("owns", []))
        if event.get("parent"):
            participant(state, event["parent"])
        prior = next((p for p in state["participants"] if p["participant"] == event["participant"]), None)
        if prior and any(prior.get(k) != event.get(k) for k in ("provider", "role", "model", "parent")):
            raise ValueError("participant identity is immutable; use a new participant id")
        if prior and all(prior.get(k) == v for k, v in event.items()) and prior["joined"]:
            return False
        if prior and prior["joined"]:
            raise ValueError("leave before changing participant metadata")
        if prior is None:
            called = bool(event.get("parent"))
            peers = sum(bool(p.get("parent")) == called for p in state["participants"])
            if peers >= (MAX_CALLS if called else MAX_PARTICIPANTS):
                raise ValueError("room call limit reached; start a new bounded room" if called
                                 else "room participant limit reached")
    elif kind == "leave":
        participant(state, event.get("participant"))
    elif kind == "message":
        identifier(event.get("id"), "message")
        participant(state, event.get("sender"))
        recipient = event.get("recipient")
        if recipient != "all":
            participant(state, recipient)
        if recipient == event["sender"]:
            raise ValueError("a message needs another participant or all")
        if "audience" in event:
            if recipient != "all" or event.get("message_kind") == "status" or event["sender"] in event["audience"]:
                raise ValueError("a carried audience belongs to a broadcast")
            for who in event["audience"]:
                participant(state, who)
        if event.get("message_kind") not in KINDS:
            raise ValueError("invalid message kind")
        if str(event["id"]).startswith("result-") and (
                event["id"] != "result-" + event["sender"] or event["message_kind"] != "handoff"
                or participant(state, event["sender"]).get("parent") != recipient):
            # result-<call> ids are reserved for a call's own handoff to its parent, which settles it on the board.
            raise ValueError("result-<participant> ids are reserved for that participant's handoff to its parent")
        if str(event["id"]).startswith("rollover-unread-") and not carry:
            # The rollover's note that unread mail stayed behind is skipped when its id exists, so nobody else takes it.
            raise ValueError("rollover-unread- ids are reserved for a rollover's note")
        old = next((m for m in state["messages"] if m["id"] == event["id"]), None)
        if not old and message_capacity_reached(state):
            raise ValueError("room message limit reached; start a new bounded room")
        if old:
            if all(old.get(k) == v for k, v in event.items()) and old["text"] == text:
                return False
            raise ValueError("message id conflicts with its existing content")
        if event.get("reply_to"):
            original = next((m for m in state["messages"] if m["id"] == event["reply_to"]), None)
            if (carry and original and event["sender"] not in original["targets"] and repo is not None
                    and state["predecessor"]):
                # A broadcast carried before its finished call answered it holds the audience carried then; the
                # sealed log it was sent in holds the one it was sent to, however many rollovers narrowed it since.
                for earlier in _lineage(repo, state["id"]):
                    original = next((m for m in project(records(repo, earlier))["messages"]
                                     if m["id"] == event["reply_to"]), None)
                    if not original or event["sender"] in original["targets"]:
                        break
            if (not original or event["sender"] not in original["targets"]
                    or event["recipient"] != original["sender"]):
                raise ValueError("reply must return to the sender of a message addressed to this participant")
    else:
        participant(state, event.get("participant"))
        mids = event.get("messages")
        if not isinstance(mids, list) or not 1 <= len(mids) <= 200 or any(not isinstance(m, str) for m in mids):
            raise ValueError("ack requires bounded message ids")
        indexed = {m["id"]: m for m in state["messages"]}
        # Unread mail a rollover left in a sealed log is acknowledged here; the sealed log takes no rows.
        lineage = (_lineage_messages(repo, state, set(mids) - set(indexed))
                   if repo is not None and state["predecessor"] else {})
        for mid, found in lineage.items():
            indexed[mid] = dict(found, received_by=[
                row["room_event"]["participant"] for row in rows
                if row.get("room_event", {}).get("kind") == "received" and mid in row["room_event"]["messages"]])
        for mid in mids:
            original = indexed.get(mid)
            if not original or event["participant"] not in original["targets"]:
                raise ValueError("cannot acknowledge a message addressed to another participant")
        if all(event["participant"] in indexed[mid]["received_by"] for mid in mids):
            return False
    return True


_CARRY_ENV = ("OMS_ROOM_CARRY", "OMS_ROOM_CARRY_TS")
_SESSION_ENV = ("OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "OMS_ROOM_REPO")


def command(repo, *args, env=None):
    run = subprocess.run(["bash", str(ENTRY), "thread", "--repo", str(repo)] + list(args), env=env,
                         capture_output=True, text=True, check=False, timeout=15, stdin=subprocess.DEVNULL)
    if run.returncode:
        raise ValueError(clean(run.stderr or run.stdout, 300))
    return run.stdout.strip()


def _write(repo, room, event, text, carried=None):
    """carried: None for the session's own write; True, or the original row time, for a rollover's carry.

    A carry replays evidence from a sealed log, so it claims no session binding; it runs only where a rollover
    may (never in a worker), under the sealed room's rollover lock."""
    env = {k: v for k, v in os.environ.items() if k not in _CARRY_ENV}
    if carried is not None:
        env = {k: v for k, v in env.items() if k not in _SESSION_ENV}
        env["OMS_ROOM_CARRY"] = "1"
        if isinstance(carried, str):
            env["OMS_ROOM_CARRY_TS"] = carried
    return command(repo, "append", "--id", identifier(room, "room"), "--role", "note", "--text", text,
                   "--room-event", json.dumps(event, ensure_ascii=False), env=env)


def _rollover_lock(repo, old, timeout=30):
    from panel_results import _lock
    name = "room-rollover-" + old
    if len(name) > 160:
        name = "room-rollover-" + hashlib.sha256(old.encode("utf-8")).hexdigest()[:32]
    return _lock(repo, name, timeout=timeout)


def append(repo, room, event, text):
    """Write to the room an id now names; a writer that raced a rollover lets its carry finish and retries once."""
    literal = identifier(room, "room")
    try:
        return _write(repo, resolve(repo, literal), event, text)
    except ValueError as error:
        unfinished = "room carry is unfinished" in str(error)
        if "room continues as" not in str(error) and not unfinished:
            raise
        target = settle(repo, literal)
        if target == literal and not unfinished:
            raise
    return _write(repo, target, event, text)


def create(repo, room=None, title="Shared work", task_id=None, creator=None, predecessor=None):
    room = identifier(room or "room-" + uuid.uuid4().hex[:12], "room")
    if task_id:
        identifier(task_id, "task")
    if not title.strip() or len(title.encode("utf-8")) > 1000:
        raise ValueError("room title needs 1..1000 bytes")
    event = _created_event(task_id, creator, predecessor)
    validate_event([{"live": True, "thread": room}], event, title)
    if (Path(repo) / ".oms/threads" / (room + ".jsonl")).exists():
        raise ValueError("room id already exists; join it explicitly")
    command(repo, "new", "--id", room, "--live", "--topic", title)
    _write(repo, room, event, title)
    return room


def _created_event(task_id, creator, predecessor):
    return {"kind": "created", **({"task_id": task_id} if task_id else {}),
            **({"participant": identifier(creator, "creator")} if creator else {}),
            **({"predecessor": identifier(predecessor, "predecessor room")} if predecessor else {})}


def carry_plan(state, current=None, bounded=True):
    """(writes, dropped): the (event, text, time) writes that keep a sealed room's live state working in current.

    Essential state always travels: joined mains, calls still running or holding unread results with their
    results and direct mail, open questions (a call's oldest one) and each main's latest status. Other unread
    mail fills the message reserve newest first; the rest stays readable in the sealed log. What current already
    holds is skipped, and its members are brought up to date with the sealed log."""
    joined = {p["participant"]: p for p in state["participants"] if p["joined"]}

    def rooted(who):
        parent, seen = joined[who].get("parent"), {who}
        while parent:
            if parent not in joined or parent in seen:
                return False
            seen.add(parent)
            parent = joined[parent].get("parent")
        return True
    # A call whose parent left has nobody to hand its result to; its mail stays in the sealed log.
    members = {who: p for who, p in joined.items() if rooted(who)}
    results = {m["id"]: m for m in state["messages"] if str(m["id"]).startswith("result-")}
    there = {p["participant"]: p for p in (current or {}).get("participants", [])}
    present = {who for who, p in there.items() if p["joined"]}
    keep = {who for who, p in members.items() if not p.get("parent")
            or "result-" + who not in results or results["result-" + who]["pending_for"]}
    keep |= present & set(members)
    # A call that already handed back its result reads no more mail; its Assigned handoff stays behind.
    finished = {who for who in members if "result-" + who in results}
    questions = [q["id"] for q in open_questions(state)]
    indexed = {m["id"]: m for m in state["messages"]}
    known = {m["id"]: m for m in (current or {}).get("messages", [])}
    while True:
        chosen, essential = {}, set()
        for m in state["messages"]:
            if set(m["pending_for"]) & (keep - finished):
                chosen[m["id"]] = m
                if m["recipient"] in keep - finished and members[m["recipient"]].get("parent"):
                    essential.add(m["id"])
        # A carried call keeps its result, so the successor never shows it running again.
        essential.update("result-" + who for who in keep if "result-" + who in results)
        asked = set()
        for qid in questions:
            sender = indexed[qid]["sender"]
            if not set(indexed[qid]["targets"]) & keep or members.get(sender, {}).get("parent") and sender in asked:
                continue
            # A call's later open questions are ordinary mail, so a flood of them cannot block the rollover.
            asked.add(sender)
            essential.add(qid)
        latest = {}
        for m in state["messages"]:
            if m["message_kind"] == "status" and m["sender"] in keep and members[m["sender"]]["role"] == "main":
                latest[m["sender"]] = m["id"]
        essential.update(latest.values())
        chosen.update((m["id"], m) for m in state["messages"] if m["id"] in essential)
        chosen = {mid: m for mid, m in chosen.items() if m["sender"] in members
                  and (m["recipient"] == "all" or m["recipient"] in members)}
        essential &= set(chosen)
        optional = sorted((m for mid, m in chosen.items() if mid not in essential), key=lambda m: -m["seq"])
        dropped = optional[max(0, MESSAGE_RESERVE - len(essential)):]
        for m in dropped:
            del chosen[m["id"]]
        # A carried question that was answered travels with its first answer, so it does not open again.
        answered = {m.get("reply_to") for m in list(chosen.values()) + list(known.values())
                    if m["message_kind"] == "answer"}
        for m in state["messages"]:
            if (m["message_kind"] == "answer" and m.get("reply_to") not in answered and m["sender"] in members
                    and m["recipient"] in members and (m.get("reply_to") in chosen or m.get("reply_to") in known)):
                chosen[m["id"]] = m
                answered.add(m["reply_to"])
        grown = set(keep)
        grown.update(m["sender"] for m in chosen.values())
        grown.update(m["recipient"] for m in chosen.values() if m["recipient"] != "all")
        for who in list(grown):
            parent = members[who].get("parent")
            while parent and parent not in grown:
                grown.add(parent)
                parent = members[parent].get("parent")
        if grown == keep:
            break
        keep = grown
    if bounded and (len(essential) > CARRY_MESSAGES or sum(bool(members[w].get("parent")) for w in keep) > CARRY_CALLS
                    or sum(not members[w].get("parent") for w in keep) > MAX_PARTICIPANTS):
        raise ValueError("rollover carry exceeds reserve")
    order, placed = [], set()

    def place(who):
        parent = members[who].get("parent")
        if who in placed:
            return
        if parent:
            place(parent)
        placed.add(who)
        order.append(members[who])
    for who in sorted(keep, key=lambda w: members[w]["seq"]):
        place(who)
    # Members that left the sealed log leave its successor first, freeing a native session another member now
    # holds; nothing carried names them, as every carried member and message is joined in the sealed log.
    gone = sorted(present - set(joined), key=lambda w: there[w]["seq"])
    writes = [({"kind": "leave", "participant": who}, "Participant left", None) for who in gone]
    held = {members[w].get("consumer") for w in keep} | {
        p.get("consumer") for w, p in there.items() if p["joined"] and w not in gone}
    # Sessions each member holds in current as the writes so far leave it: a member waits while another still
    # holds the session it moves to, so a session handed from one member to another is released before it is taken.
    holding = {w: p.get("consumer") for w, p in there.items() if p["joined"] and w not in gone}
    queue = []
    for p in order:
        who = p["participant"]
        # Only a Codex/Claude main takes a native bind; any other member, or one left without a session, rejoins.
        bindable = p["role"] == "main" and p["provider"] in {"codex", "claude"}
        now = there.get(who) if who in present else None
        if now is not None and now.get("consumer") != p.get("consumer") and (not bindable or not p.get("consumer")):
            writes.append(({"kind": "leave", "participant": who}, "Participant left", None))
            holding.pop(who, None)
            now = None
        queue.append((p, now, bindable))

    def ready(entry):
        p, now, _ = entry
        taken = {c for w, c in holding.items() if c and w != p["participant"]}
        return p.get("consumer") not in taken and (now is not None or not p.get("parent") or p["parent"] in holding)
    while queue:
        # With no member ready the sessions form a cycle; its first write is refused as before.
        p, now, bindable = queue.pop(next((i for i, entry in enumerate(queue) if ready(entry)), 0))
        who = p["participant"]
        holding[who] = p.get("consumer")
        if now is None:
            event = {"kind": "join", "participant": who, "provider": p["provider"], "role": p["role"],
                     "model": p.get("model"), "label": p["label"], "owns": list(p.get("owns", []))}
            if p.get("parent"):
                event["parent"] = p["parent"]
            # A rebound main joins with the session it first joined with and is rebound below, so resume ownership
            # still recognizes that session; unless another carried member now holds it.
            first = p.get("initial_consumer")
            consumer = (first if bindable and p.get("consumer") and first and first not in held - {p["consumer"]}
                        else p.get("consumer"))
            if consumer:
                event["consumer"] = consumer
            writes.append((event, "Joined: " + p["label"], None))
            now = dict(event)
        if p.get("label_override") and now.get("label_override") != p["label_override"]:
            # The board titles a main by its described work, which a plain join label does not carry.
            writes.append(({"kind": "describe", "participant": who, "label": p["label_override"]},
                           "Work description updated", None))
        if list(now.get("owns", [])) != list(p.get("owns", [])):
            writes.append(({"kind": "scope", "participant": who, "owns": list(p.get("owns", []))},
                           "Scope: " + (", ".join(p.get("owns", [])) or "cleared"), None))
        if p.get("consumer") and now.get("consumer") != p["consumer"]:
            event = {"kind": "bind", "participant": who, "consumer": p["consumer"]}
            if now.get("consumer"):
                event["previous"] = now["consumer"]
            writes.append((event, "Native session rebound", None))
    for m in sorted(chosen.values(), key=lambda m: m["seq"]):
        if m["id"] in known:
            continue
        event = {"kind": "message", "id": m["id"], "sender": m["sender"], "recipient": m["recipient"],
                 "message_kind": m["message_kind"]}
        if m.get("reply_to") and (m["reply_to"] in chosen or m["reply_to"] in known):
            event["reply_to"] = m["reply_to"]
        if m["recipient"] == "all" and m["message_kind"] != "status":
            # A broadcast reaches only who it reached when sent, not everyone carried after it.
            event["audience"] = [who for who in m["targets"] if who in keep]
        writes.append((event, m["text"], m.get("ts")))
    receipts = {}
    for m in state["messages"]:
        if m["id"] not in chosen and m["id"] not in known:
            continue
        # Mail read between the first carry and the seal is already in the successor; its receipt follows it.
        done = set(known.get(m["id"], {}).get("received_by", []))
        targets = known[m["id"]]["targets"] if m["id"] in known else m["received_by"]
        for who in m["received_by"]:
            if who in keep and who not in done and who in targets:
                receipts.setdefault(who, []).append(m["id"])
    for who, mids in sorted(receipts.items()):
        for start in range(0, len(mids), 200):
            writes.append(({"kind": "received", "participant": who, "messages": mids[start:start + 200]},
                           "Messages consumed before rollover", None))
    return writes, [m for m in dropped if m["id"] not in known]


def carry(repo, state, new, bounded=True):
    """Write the carry plan into new; returns the unread messages that stay only in the sealed log."""
    writes, dropped = carry_plan(state, project(records(repo, new)), bounded)
    for event, text, stamp in writes:
        _write(repo, new, event, text, carried=stamp or True)
    return dropped


_CARRIED = set()


def _carried(repo, room):
    key = (str(repo), room)
    if key not in _CARRIED and project(records(repo, room))["carried"]:
        _CARRIED.add(key)  # a completed carry never reopens
    return key in _CARRIED


def _complete(repo, old, new):
    """Finish the carry from sealed old into new and mark it complete; the caller holds old's rollover lock."""
    if _carried(repo, new):
        return
    rows = records(repo, old)
    seal = rows[-1].get("room_event", {})
    if seal.get("kind") != "successor" or seal.get("target") != new:
        raise ValueError("room %s is not sealed into %s" % (old, new))
    dropped = carry(repo, project(rows), new, bounded=False)
    note = "rollover-unread-" + hashlib.sha256(old.encode("utf-8")).hexdigest()[:12]
    if dropped and note not in {m["id"] for m in project(records(repo, new))["messages"]}:
        # One note per sealed room, from the main its seal names, so a rerun by another main writes the same one.
        _write(repo, new, {"kind": "message", "id": note, "sender": seal["participant"], "recipient": "all",
                           "message_kind": "note"},
               "Older unread mail stays in %s; read it with oms room show --id %s --exact" % (old, old), carried=True)
    _write(repo, new, {"kind": "carried"}, "Carried from " + old, carried=True)
    _CARRIED.add((str(repo), new))


def _finish_carry(repo, room):
    """Complete the carry into room once its predecessor is sealed into it; a worker only waits for one running."""
    old = _predecessor(repo, room)
    if not old or (str(repo), room) in _CARRIED or _tail_successor(repo, old) != room:
        return
    with _rollover_lock(repo, old):
        if not _child():
            _complete(repo, old, room)


def settle(repo, room):
    """The room an id names, once the rollover that sealed it into that room has finished its carry.

    A rollover cut short after its seal leaves the successor refusing every write but its carry; the first
    writer other than a worker to meet that refusal finishes the carry here and retries."""
    target = resolve(repo, room, fresh=True)
    _finish_carry(repo, target)
    return target


def rollover(repo, old, actor=None):
    """Seal a filling room and continue it as its derived successor; idempotent and crash-safe.

    The bulk of the carry lands before the seal, while writers still use the old log; the successor row on the
    old log is the commit point, and the carry after it brings the successor up to the sealed log and marks it
    complete. Until then the successor refuses every other write, so a rerun only ever compares carried state."""
    if _child():
        raise ValueError("a worker cannot roll a room over")
    old = identifier(old, "room")
    _finish_carry(repo, old)  # a room is sealed only once its own carry is complete
    with _rollover_lock(repo, old):
        current = resolve(repo, old, fresh=True)
        new = successor_id(old)
        if current != old:
            if _tail_successor(repo, old) == new and _predecessor(repo, new) == old:
                _complete(repo, old, new)
            return current
        state = project(records(repo, old))
        if state["closed"]:
            raise ValueError("room is closed")
        mains = sorted((p for p in state["participants"] if p["joined"] and p["role"] == "main"
                        and not p.get("parent")), key=lambda p: p["seq"])
        chosen = next((p for p in mains if p["participant"] == actor), None) or next(iter(mains), None)
        if chosen is None:
            raise ValueError("rollover needs an enrolled main")
        carry_plan(state)
        path = Path(repo) / ".oms/threads" / (new + ".jsonl")
        if path.exists() and not path.stat().st_size:
            path.unlink()  # a successor create cut short before its header row
        # The successor keeps the sealed room's creator, whose close right does not pass to the rollover's main.
        if not path.exists():
            create(repo, new, state["title"], state["task_id"], state["creator"], predecessor=old)
        else:
            rows = records(repo, new)
            if len(rows) == 1 and rows[0].get("live") is True and not rows[0].get("room_event"):
                _write(repo, new, _created_event(state["task_id"], state["creator"], old), state["title"])
            elif _predecessor(repo, new) != old:
                raise ValueError("successor room id collision")
        if not _carried(repo, new):
            carry(repo, state, new)
        _write(repo, old, {"kind": "successor", "participant": chosen["participant"], "target": new},
               "Room continues as " + new)
        _RECENT.clear()  # this process follows the seal at once; others within RESOLVE_TTL
        _complete(repo, old, new)
        return new


def join(repo, room, who, provider, role="main", model=None, label=None, native_session=None, owns=None, parent=None):
    result = subprocess.run(["bash", "-c", '. "$1"; oms_provider_normalize "$2"', "room-provider",
                             str(ROOT / "scripts/lib/provider-registry.sh"), provider],
                            capture_output=True, text=True, check=False, timeout=5, stdin=subprocess.DEVNULL)
    if result.returncode:
        raise ValueError("join needs a registered provider or adapter")
    provider = result.stdout.strip()
    if provider == "claude" and native_session == "current":
        from claude_app_notify import current_session
        native_session = current_session()
    event = {"kind": "join", "participant": identifier(who, "participant"), "provider": provider,
             "role": role, "model": model, "label": label or who, "owns": owns or []}
    if native_session:
        event["consumer"] = hashlib.sha256(native_session.encode("utf-8")).hexdigest()[:32]
    if parent:
        event["parent"] = parent
    append(repo, room, event, "Joined: " + event["label"])
    return event


def close(repo, room, who):
    event = {"kind": "closed", "participant": identifier(who, "participant")}
    literal = identifier(room, "room")
    # Behind the rollover lock a close lands before a seal or in the successor, never between carry and seal. A
    # rollover that sealed the room while this waited moves the close to its successor's lock: the sealed room's
    # lock is held here and cannot be taken again to finish its carry.
    for _ in range(MAX_HOPS):
        target = resolve(repo, literal, fresh=True)
        with _rollover_lock(repo, target):
            if resolve(repo, literal, fresh=True) == target:
                append(repo, target, event, "Room closed")
                return {"status": "closed"}
    raise ValueError("room forwarding chain too long")


def describe(repo, room, who, label):
    """Change declared work without moving native identity or the mailbox anchor."""
    event = {"kind": "describe", "participant": identifier(who, "participant"), "label": label}
    append(repo, room, event, "Work description updated")
    return event


def now():
    return time.time()


def age(stamp):
    """Seconds since a room timestamp; 0 when it cannot be read."""
    try:
        return max(0, int(now() - calendar.timegm(time.strptime(str(stamp)[:19], "%Y-%m-%dT%H:%M:%S"))))
    except ValueError:
        return 0


def open_questions(state, recipient=None, asker=None):
    """Unanswered questions to a joined participant, oldest first; a reply_to answer closes one for everyone."""
    live = {p["participant"] for p in state["participants"] if p["joined"]}
    return [{"id": m["id"], "sender": m["sender"], "recipient": m["recipient"], "ts": m["ts"], "age": age(m["ts"])}
            for m in state["messages"]
            if m["message_kind"] == "question" and not m["answered"] and any(t in live for t in m["targets"])
            and (recipient is None or recipient in m["targets"]) and (asker is None or m["sender"] == asker)]


def reminder_id(question, tier):
    legacy = "remind-%s-%s" % (question, tier)
    if len(legacy) <= 160:
        return legacy
    return "reminder-%s-%s" % (hashlib.sha256(question.encode("utf-8")).hexdigest(), tier)


def remind(repo, room, who, state=None):
    """Record the asker's own reminder note for its open question; one per tier, ids make a repeat a no-op."""
    room = resolve(repo, room)
    state = state or project(records(repo, room))
    seen, sent = {m["id"] for m in state["messages"]}, []
    attempts = 0
    if len(state["messages"]) >= MAX_MESSAGES:
        return sent
    for question in open_questions(state, asker=who):
        tier = 2 if question["age"] >= SECOND_REMINDER_AFTER else 1 if question["age"] >= ASKER_AFTER else 0
        reminder = reminder_id(question["id"], tier)
        if not tier or question["recipient"] == "all" or reminder in seen:
            continue
        if attempts >= MAX_REMINDERS:
            break
        attempts += 1
        try:
            send(repo, room, who, question["recipient"], "Reminder: open question %s (%d min)" % (
                question["id"], question["age"] // 60), "note", message_id=reminder, linked=False)
            sent.append(question | {"reminder": reminder})
            seen.add(reminder)
        except ValueError:
            pass  # A lost race or a full room remains pending for the next safe point.
    return sent


def _settled_reply(repo, room, who, recipient, reply_to):
    """reply_to, or None for a reply to mail that was settled before its room rolled over and so stayed behind."""
    here = resolve(repo, room)
    if not _predecessor(repo, here) or any(m["id"] == reply_to for m in project(records(repo, here))["messages"]):
        return reply_to
    for earlier in _lineage(repo, here):
        original = next((m for m in project(records(repo, earlier))["messages"] if m["id"] == reply_to), None)
        if original:
            if who in original["targets"] and recipient == original["sender"]:
                print("room: %s stayed in %s when it rolled over; sending without --reply-to" % (reply_to, earlier),
                      file=sys.stderr)
                return None
            break
    return reply_to


def send(repo, room, who, recipient, text, kind="note", reply_to=None, message_id=None, linked=True):
    if not isinstance(text, str) or not text.strip() or len(text.encode("utf-8")) > 4000:
        raise ValueError("message needs 1..4000 UTF-8 bytes")
    if kind == "answer" and not reply_to and linked:
        # An answer without --reply-to never closes its question; link the one it can only mean.
        pending = [q["id"] for q in open_questions(project(records(repo, resolve(repo, room))), recipient=who)
                   if q["sender"] == recipient]
        if len(pending) > 1:
            raise ValueError("%s has %d open questions to you; choose one with --reply-to: %s" % (
                recipient, len(pending), ", ".join(pending[:8])))
        if pending:
            reply_to = pending[0]
            print("room: linked this answer to open question %s" % reply_to, file=sys.stderr)
    if reply_to:
        reply_to = _settled_reply(repo, room, who, recipient, reply_to)
    event = {"kind": "message", "id": message_id or "msg-" + uuid.uuid4().hex,
             "sender": who, "recipient": recipient, "message_kind": kind}
    if reply_to:
        event["reply_to"] = reply_to
    try:
        append(repo, room, event, text)
    except ValueError as error:
        if "room message limit reached" not in str(error) or _child():
            raise
        # A full room rolls over rather than dropping the message, e.g. a call's result handoff.
        rollover(repo, resolve(repo, room, fresh=True))
        append(repo, room, event, text)
    return event


def bind(repo, ident, who, native_session, replaces=None):
    """Bind a main to its native session; replaces names the consumer hash a rebind moves away from."""
    if native_session == "current":
        if participant(status(repo, ident), who)["provider"] != "claude":
            raise ValueError("current shortcut is for the calling Claude session")
        from claude_app_notify import current_session
        native_session = current_session()
    identifier(native_session, "native session")
    event = {"kind": "bind", "participant": who,
             "consumer": hashlib.sha256(native_session.encode()).hexdigest()[:32]}
    if replaces:
        event["previous"] = replaces
    append(repo, ident, event, "Native session rebound" if replaces else "Native session bound")
    return event


def scope(repo, ident, who, owns):
    """Replace who's declared scopes; an empty list clears them. Declarations coordinate, they never lock."""
    event = {"kind": "scope", "participant": identifier(who, "participant"), "owns": list(owns)}
    append(repo, ident, event, "Scope: " + (", ".join(owns) or "cleared"))
    return event


def visible(row, who, joined_seq=0):
    # A carried broadcast follows every carried join, so its audience, not the join order, says who it reached.
    event = row.get("room_event", {})
    return (event.get("kind") == "message" and event.get("message_kind") != "status"
            and row.get("seq", 0) > joined_seq and event.get("sender") != who
            and event.get("recipient") in ("all", who) and who in event.get("audience", [who]))


def updates(repo, room, who, after="", budget=8192, limit=50, *, state=None):
    if (type(budget) is not int or not 1 <= budget <= thread_live.MAX_ROW
            or type(limit) is not int or not 1 <= limit <= 200):
        raise ValueError("updates requires bytes 1..65536 and turns 1..200")
    room = resolve(repo, room)
    state = state if state is not None else project(records(repo, room))
    member = participant(state, who)
    if after and _cursor_room(after) not in (None, room) and _cursor_room(after) in _lineage(repo, room):
        print("room: %s continues as %s; reading unread mail from its start" % (_cursor_room(after), room),
              file=sys.stderr)
        after = ""
    if after:
        delta = thread_live.updates(repo, room, after, budget, limit, allow_first_row_over_budget=True)
        return dict(delta, turns=[r for r in delta["turns"] if visible(r, who, member["seq"])])

    known = {message["id"] for message in state["messages"]}
    unread = {message["id"] for message in state["messages"]
              if who in message["targets"] and who not in message["received_by"]}
    with thread_live.open_thread(repo, room) as handle:
        scanned = 0
        enrollment = None
        while scanned <= thread_live.MAX_FILE:
            line = handle.readline(thread_live.MAX_ROW + 1)
            scanned += len(line)
            if not line or not line.endswith(b"\n") or len(line) > thread_live.MAX_ROW or scanned > thread_live.MAX_FILE:
                raise ValueError("room enrollment turn is unavailable")
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError("invalid room row")
            if row.get("seq") == member["seq"]:
                if (row.get("thread") != room or row.get("room_event", {}).get("kind") != "join"
                        or row["room_event"].get("participant") != who):
                    raise ValueError("room enrollment turn changed")
                enrollment = thread_live.cursor_for(handle, room, handle.tell())
                break
        if enrollment is None:
            raise ValueError("room enrollment turn is unavailable")

        turns, used = [], 0
        cursor = enrollment
        more_unread = False
        while scanned <= thread_live.MAX_FILE:
            offset = handle.tell()
            line = handle.readline(thread_live.MAX_ROW + 1)
            if not line:
                cursor = thread_live.cursor_for(handle, room, offset)
                break
            scanned += len(line)
            if not line.endswith(b"\n") or len(line) > thread_live.MAX_ROW or scanned > thread_live.MAX_FILE:
                raise ValueError("room log is oversized or has an incomplete turn")
            row = json.loads(line)
            if not isinstance(row, dict) or row.get("thread") != room:
                raise ValueError("invalid room row")
            event = row.get("room_event", {})
            # A cached projection cannot classify a new message's audience or
            # receipts. Leave it for the next read instead of skipping it.
            if event.get("kind") == "message" and event.get("id") not in known:
                cursor = thread_live.cursor_for(handle, room, offset)
                more_unread = True
                break
            if visible(row, who, member["seq"]) and event.get("id") in unread:
                if len(turns) >= limit or (turns and used + len(line) > budget):
                    more_unread = True
                    break
                turns.append(row)
                used += len(line)
                cursor = thread_live.cursor_for(handle, room, handle.tell())
        else:
            raise ValueError("room log exceeds the bounded file size")
        return {"schema": 1, "thread": room, "turns": turns,
                "cursor": cursor, "has_more": more_unread}


def _cursor_room(cursor):
    try:
        value = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
        return value[1] if isinstance(value, list) and len(value) > 1 and isinstance(value[1], str) else None
    except (ValueError, TypeError):
        return None


def _lineage(repo, room):
    """Predecessor ids of a room, newest first, bounded like resolve()."""
    found, cur = [], room
    for _ in range(MAX_HOPS):
        cur = _predecessor(repo, cur)
        if not cur or cur in found or cur == room:
            break
        found.append(cur)
    return found


def acknowledge(repo, room, who, mids):
    room = resolve(repo, room)
    state = project(records(repo, room))
    if state["predecessor"]:
        # Mail already consumed before a rollover was not carried; acknowledging it again is a no-op. Unread mail
        # the rollover left in the sealed log is acknowledged here; any other id is refused as usual.
        known = {m["id"] for m in state["messages"]}
        lineage = _lineage_messages(repo, state, set(mids) - known)
        settled = [mid for mid in mids if who in lineage.get(mid, {}).get("received_by", [])]
        if settled:
            print("room: already consumed before rollover: " + ", ".join(settled[:8]), file=sys.stderr)
        mids = [mid for mid in mids if mid not in settled]
        if not mids:
            return
    append(repo, room, {"kind": "received", "participant": who, "messages": mids}, "Messages consumed")


def _lineage_messages(repo, state, mids):
    """{id: message} for ids a rollover left in this room's sealed predecessors, nearest first as each log projects
    them; each log is read once however many ids are asked, since a worker's acknowledgment names up to 200."""
    wanted, found = set(mids), {}
    for earlier in _lineage(repo, state["id"]):
        if not wanted:
            break
        for m in project(records(repo, earlier))["messages"]:
            if m["id"] in wanted:
                found[m["id"]] = m
                wanted.discard(m["id"])
    return found


def selected(repo, consumer=None, preferred=None, who=None):
    """Explicit binding wins; otherwise match a native session to its latest join."""
    return _selected(repo, consumer, preferred, who)[0]


def _selected(repo, consumer=None, preferred=None, who=None):
    """Retain the selected projection for a hook's delivery and reminder checks."""
    if preferred:
        state = project(records(repo, resolve(repo, identifier(preferred, "room"))))
        if state["closed"]:
            return None, state
        member = participant(state, who) if who else next((p for p in state["participants"]
                     if p.get("consumer") == consumer and p["joined"]), None)
        return ((state["id"], member["participant"]) if member else None), state
    folder = Path(repo) / ".oms/threads"
    if not consumer or not folder.is_dir():
        return None, None
    candidates, enrolled = [], False
    states, incomplete = discover(repo)
    if incomplete:
        return (None, None), None
    for state in states:
        enrolled = enrolled or any(consumer in (p.get("consumer"), p.get("initial_consumer")) for p in state["participants"])
        if state["closed"] or state.get("sealed"):
            continue
        for member in state["participants"]:
            if member.get("consumer") == consumer and member["joined"]:
                candidates.append((member["joined_at"], member["seq"], state["id"], member["participant"], state))
    return (candidates[0][2:4], candidates[0][4]) if len(candidates) == 1 else ((None, None) if enrolled else None, None)


def messages(repo, room):
    """Every projected message of the room, oldest first; reads nothing else and writes nothing."""
    return project(records(repo, resolve(repo, room)))["messages"]


def status(repo, room, exact=False):
    state = project(records(repo, room if exact else resolve(repo, room)))
    if state["successor"]:
        _later_receipts(repo, state)
    state["message_count"] = len(state["messages"])
    live = {p["participant"] for p in state["participants"] if p["joined"]}
    state["pending_count"] = sum(len(live.intersection(m["pending_for"])) for m in state["messages"])
    state["statuses"] = {m["sender"]: {"text": m["text"], "ts": m["ts"], "seq": m["seq"]}
                         for m in state["messages"] if m["message_kind"] == "status"}
    state["open_questions"] = [{k: q[k] for k in ("id", "sender", "recipient", "ts")}
                               for q in open_questions(state)[:32]]
    state["call_results"] = {}
    parents = {p["participant"]: p.get("parent") for p in state["participants"] if p.get("role") != "main"}
    for m in state["messages"]:
        found = re.match(r"result-(.+)\Z", str(m["id"]))
        # Only a call's own handoff to its declared parent counts; another sender cannot settle it.
        if (found and m["sender"] == found.group(1) and m["message_kind"] == "handoff"
                and found.group(1) in parents and parents[found.group(1)] == m["recipient"]):
            exited = re.match(r"Call exit=(\d{1,4})\b", str(m["text"]))
            state["call_results"][found.group(1)] = {"exit": int(exited.group(1)) if exited else None,
                                                     "ts": m["ts"], "seq": m["seq"],
                                                     "read": not m["pending_for"]}
    state["received_count"] = sum(len(m["received_by"]) for m in state["messages"])
    state["answered_count"] = sum(m["answered"] for m in state["messages"])
    pairs = {}
    for message in state["messages"]:
        for target in message["targets"]:
            key = (message["sender"], target)
            pair = pairs.setdefault(key, {"sender": key[0], "recipient": key[1], "sent": 0, "pending": 0})
            pair["sent"] += 1
            pair["pending"] += int(target in message["pending_for"])
    state["pairs"] = list(pairs.values())
    # Live readers are known only to the panel; retain pending mail beyond the recent text window.
    # Inbox previews need routing and a bounded snippet, not the full pending message bodies.
    pending = [dict({k: message[k] for k in ("id", "pending_for", "sender", "recipient", "message_kind", "answered", "ts")},
                    text=message["text"][:300])
               for message in state["messages"] if message["pending_for"]]
    state["pending_messages"] = pending[-500:]
    state["pending_messages_truncated"] = len(pending) > 500
    state["messages"] = state["messages"][-12:]
    return state


def _later_receipts(repo, state):
    """Apply to a sealed log the receipts its successors took for the unread mail it kept."""
    pending = {m["id"]: m for m in state["messages"] if m["pending_for"]}
    cur, seen = state["id"], {state["id"]}
    for _ in range(MAX_HOPS):
        nxt = _tail_successor(repo, cur) if cur != state["id"] else state["successor"]
        if not pending or not nxt or nxt in seen or _predecessor(repo, nxt) != cur:
            return
        for row in records(repo, nxt):
            event = row.get("room_event", {})
            if event.get("kind") != "received":
                continue
            for mid in event["messages"]:
                if mid in pending and event["participant"] in pending[mid]["targets"]:
                    message = pending[mid]
                    message["received_by"] = sorted(set(message["received_by"]) | {event["participant"]})
                    message["pending_for"] = [p for p in message["pending_for"] if p != event["participant"]]
        seen.add(nxt)
        cur = nxt


_PREDECESSOR_CALLS = {}


def predecessor_calls(repo, state):
    """Settled calls of the room this one continues, shaped like board call nodes; sealed logs are read once."""
    old = state.get("predecessor")
    if not old:
        return []
    try:
        info = thread_live.thread_path(repo, old).stat()
        stamp = (info.st_dev, info.st_ino, info.st_size)
        cached = _PREDECESSOR_CALLS.get((str(repo), old))
        if not cached or cached[0] != stamp:
            before = status(repo, old, exact=True)
            calls = []
            for member in before["participants"]:
                result = before["call_results"].get(member["participant"])
                if not member.get("parent") or not result:
                    continue
                code = result["exit"]
                calls.append(dict(member, state="done" if code == 0 else "failed", ended=result["ts"],
                                  exit=code, attempt=None, title=member.get("label"), _predecessor=old))
            cached = _PREDECESSOR_CALLS[(str(repo), old)] = (stamp, calls)
    except (OSError, ValueError, KeyError, TypeError):
        return []
    here = {p["participant"] for p in state.get("participants", [])}
    return sorted((c for c in cached[1] if c["participant"] not in here),
                  key=lambda c: (str(c.get("ended") or ""), c.get("seq", 0)), reverse=True)


def publish(repo, ident, retry=False, app="codex", recipient=None, allow_wakeup=False):
    """Durable intent precedes delivery; uncertain sends are never retried implicitly."""
    import codex_app_notify
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("a worker cannot publish room output")
    if app not in {"codex", "claude"} or allow_wakeup and app != "claude":
        raise ValueError("invalid app delivery options")
    state = status(repo, ident)
    target, consumer = "codex", None
    if app == "claude":
        member = participant(state, recipient)
        if member["provider"] != "claude" or member["role"] != "main" or not member.get("consumer"):
            raise ValueError("Claude app delivery needs an enrolled native main")
        target, consumer = "claude." + recipient, member["consumer"]
        if not allow_wakeup:
            return {"room": ident, "target": target, "receipt": {"status": "wakeup_required",
                    "submitted": False, "delivery_unknown": False}, "deduplicated": False}
    lines = ["OMS room: " + state["title"], "Room: " + ident]
    lines += ["%s / %s / %s" % (p["label"], p["provider"], p["role"])
              for p in state["participants"] if p["joined"]]
    lines += ["%s -> %s: %s" % (m["sender"], m["recipient"], clean(m["text"], 240))
              for m in state["messages"][-4:]]
    lines += ["Consumption is not approval; call exits are not acceptance."]
    text = "\n".join(lines)
    digest = hashlib.sha256((text + (consumer or "")).encode("utf-8")).hexdigest()
    prior = state["publications"].get(target)
    if prior and not retry and (prior["status"] == "pending" or prior["delivery_unknown"]
            or prior["digest"] == digest and prior["status"] == "persisted"):
        return {"room": ident, "target": target, "receipt": prior, "deduplicated": True}
    event = {"kind": "publication", "id": "publish-" + uuid.uuid4().hex, "digest": digest,
             "status": "pending", "delivery_unknown": True, "retry": bool(retry), "target": target}
    append(repo, ident, event, "App delivery intent")
    if app == "claude":
        from claude_app_notify import deliver
        receipt = deliver(repo, text, consumer, allow_wakeup)
    else:
        receipt = codex_app_notify.deliver(repo, text)
    event.update(status=receipt["status"], delivery_unknown=bool(receipt.get("delivery_unknown")))
    if receipt.get("turn_id"):
        event["turn_id"] = receipt["turn_id"]
    append(repo, ident, event, "App delivery: " + event["status"])
    return {"room": ident, "target": target, "receipt": receipt, "deduplicated": False}


def main(argv=None):
    parser = argparse.ArgumentParser(description="OMS work rooms: shared threads, addressed messages and explicit consumption.")
    parser.add_argument("action", choices=("new", "join", "bind", "scope", "describe", "leave", "close", "send", "updates", "ack", "show", "list", "publish", "rollover"))
    parser.add_argument("--repo", default=os.environ.get("OMS_ROOM_REPO") or os.getcwd())
    parser.add_argument("--id", default=os.environ.get("OMS_ROOM_ID"))
    parser.add_argument("--participant", default=os.environ.get("OMS_ROOM_PARTICIPANT"))
    parser.add_argument("--provider", default=os.environ.get("OMS_AGENT", "codex"))
    parser.add_argument("--role", choices=sorted(ROLES), default="main")
    parser.add_argument("--model")
    parser.add_argument("--label")
    parser.add_argument("--native-session", help="native session id, or current inside Claude; stored only as a hash")
    parser.add_argument("--owns", action="append", help="declared relative scope, not a write grant or file lock")
    parser.add_argument("--clear", action="store_true", help="scope only: clear the declared scopes")
    parser.add_argument("--parent")
    parser.add_argument("--topic", default="Shared work")
    parser.add_argument("--task-id")
    parser.add_argument("--to")
    parser.add_argument("--text")
    parser.add_argument("--text-file")
    parser.add_argument("--kind", choices=sorted(KINDS), default="note")
    parser.add_argument("--reply-to")
    parser.add_argument("--message-id")
    parser.add_argument("--after", default="")
    parser.add_argument("--message", action="append", help="consumed message id for ack")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--exact", action="store_true", help="show only: read this log itself, not the room it continues as")
    parser.add_argument("--retry", action="store_true", help="publish only: explicitly retry an uncertain snapshot; may duplicate output")
    parser.add_argument("--app", choices=("codex", "claude"), help="publish target; default Codex fixed receiver")
    parser.add_argument("--allow-wakeup", action="store_true", help="Claude publish only: authorize a native inbox post which can spend model usage")
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    args = parser.parse_args(raw_argv)
    if args.to is None and args.action != "send":
        args.to = "all"
    if args.action == "describe":
        allowed_values = {"--repo", "--id", "--participant", "--label"}
        index = 0
        while index < len(raw_argv):
            word = raw_argv[index]
            if word == "describe" or word == "--json":
                index += 1
            elif word in allowed_values:
                index += 2
            elif any(word.startswith(option + "=") for option in allowed_values):
                index += 1
            else:
                parser.error("describe accepts only --repo, --id, --participant, --label and --json")
    if args.action not in {"new", "list"} and not args.id:
        parser.error("this operation needs a room id")
    if args.action in {"join", "bind", "scope", "describe", "leave", "close", "send", "updates", "ack"} and not args.participant:
        parser.error("this operation needs a participant")
    if args.action == "scope" and bool(args.owns) == args.clear:
        parser.error("scope needs --owns PATH (repeatable) or --clear")
    if args.clear and args.action != "scope":
        parser.error("--clear applies to scope only")
    if args.action == "describe" and args.label is None:
        parser.error("describe requires --label")
    if args.retry and args.action != "publish":
        parser.error("--retry applies to publish only")
    if args.app and args.action != "publish" or args.allow_wakeup and (args.action != "publish" or args.app != "claude"):
        parser.error("--app/--allow-wakeup apply to app publishing only")
    if args.app == "claude" and args.to == "all":
        parser.error("Claude app publishing needs --to PARTICIPANT")
    if args.exact and args.action != "show":
        parser.error("--exact applies to show only")
    try:
        repo = state_repository(args.repo)
        if args.id and args.action not in {"new", "rollover"} and not args.exact:
            args.id = resolve(repo, identifier(args.id, "room"))
        if args.action == "new":
            if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
                raise ValueError("a worker cannot create a room")
            result = {"id": create(repo, args.id, args.topic, args.task_id, args.participant)}
        elif args.action == "join":
            result = join(repo, args.id, args.participant, args.provider, args.role, args.model, args.label,
                          args.native_session, args.owns, args.parent)
        elif args.action == "bind":
            result = bind(repo, args.id, args.participant, args.native_session)
        elif args.action == "scope":
            result = scope(repo, args.id, args.participant, args.owns or [])
        elif args.action == "describe":
            result = describe(repo, args.id, args.participant, args.label)
        elif args.action == "leave":
            append(repo, args.id, {"kind": "leave", "participant": args.participant}, "Participant left")
            result = {"status": "left"}
        elif args.action == "close":
            result = close(repo, args.id, args.participant)
        elif args.action == "send":
            if args.to is None:
                args.to = "all"
                if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
                    member = participant(project(records(repo, args.id)), os.environ.get("OMS_ROOM_PARTICIPANT"))
                    args.to = member.get("parent") or "all"
            if bool(args.text) == bool(args.text_file):
                raise ValueError("send needs exactly one text or text-file")
            text = args.text
            if args.text_file:
                text_path = Path(args.text_file)
                if not text_path.is_file() or text_path.stat().st_size > 4000:
                    raise ValueError("message file must be at most 4000 bytes")
                text = text_path.read_text(encoding="utf-8")
            if not text.strip() or len(text.encode("utf-8")) > 4000:
                raise ValueError("message needs 1..4000 UTF-8 bytes")
            result = send(repo, args.id, args.participant, args.to, text, args.kind, args.reply_to, args.message_id)
        elif args.action == "updates":
            result = updates(repo, args.id, args.participant, args.after)
        elif args.action == "ack":
            if not args.message:
                raise ValueError("ack requires the exact message ids consumed")
            acknowledge(repo, args.id, args.participant, args.message)
            result = {"status": "consumed", "approval": False}
        elif args.action == "rollover":
            if _child():
                raise ValueError("a worker cannot roll a room over")
            result = {"id": rollover(repo, args.id, args.participant), "rolled": args.id}
        elif args.action == "list":
            states, incomplete = discover(repo)
            found = [dict({k: state[k] for k in ("id", "title", "owner", "closed")}, message_count=len(state["messages"]),
                          **({"successor": state["successor"]} if state.get("sealed") else {}))
                     for state in states]
            result = {"rooms": found, "scan_limit": MAX_ROOMS, "incomplete": incomplete}
        else:
            result = status(repo, args.id, exact=args.exact)
            if args.action == "publish":
                if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
                    raise ValueError("a worker cannot publish room output")
                result = publish(repo, args.id, args.retry, args.app or "codex", args.to, args.allow_wakeup)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError, subprocess.SubprocessError) as exc:
        parser.exit(2, "error: room: " + clean(str(exc), 300) + "\n")


if __name__ == "__main__":
    sys.exit(main())
