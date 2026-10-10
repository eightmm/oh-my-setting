"""Task-scoped participation and messages over the canonical OMS thread log."""

import argparse
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
# Dispatched calls join with a parent and never free their slot; the message cap
# (two messages per call) bounds them as well.
MAX_CALLS = 480
MAX_ROOMS = 64
MAX_SCAN_ENTRIES = 1024
MAX_SCAN_BYTES = 8 * 1024 * 1024
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
                        if len(found) >= MAX_ROOMS:
                            incomplete = True
                            break
                        suffix = handle.read(MAX_SCAN_BYTES - scanned + 1)
                        scanned += len(suffix)
                        if scanned > MAX_SCAN_BYTES:
                            raise ValueError("room discovery byte limit")
                        found.append(project(decode_records(b"".join(prefix) + suffix, Path(entry.name).stem)))
                except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError):
                    incomplete = True
                    if scanned >= MAX_SCAN_BYTES:
                        break
    except FileNotFoundError:
        pass
    except OSError:
        incomplete = True
    return sorted(found, key=lambda state: state["id"]), incomplete


def event_shape(event):
    if not isinstance(event, dict):
        raise ValueError("invalid persisted room event")
    kind = event.get("kind")
    required = {"created": (), "join": ("participant", "provider", "role", "label"), "bind": ("participant", "consumer"),
                "scope": ("participant",), "describe": ("participant", "label"),
                "leave": ("participant",), "message": ("id", "sender", "recipient", "message_kind"),
                "received": ("participant",), "publication": ("id", "digest", "status")}
    if not event:
        return
    if kind not in required or any(not isinstance(event.get(key), str) for key in required[kind]):
        raise ValueError("invalid persisted room event")
    for key in ("model", "consumer", "previous", "parent", "reply_to", "task_id", "turn_id", "target"):
        if event.get(key) is not None and not isinstance(event[key], str):
            raise ValueError("invalid room event field")
    for key in ("owns", "messages"):
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
    members, messages, receipts, owner, publication, publications = {}, [], {}, None, None, {}
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
            targets = ([p for p, member in members.items() if member["joined"] and p != event["sender"]]
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
    answered = {m.get("reply_to") for m in messages if m["message_kind"] == "answer"}
    for message in messages:
        targets = message["targets"]
        message["received_by"] = sorted(receipts.get(message["id"], set()))
        message["pending_for"] = [p for p in targets if p not in receipts.get(message["id"], set())]
        message["answered"] = message["id"] in answered
    return {"schema": 1, "kind": "oms-room", "id": created["thread"],
            "title": created.get("text", "Shared work"), "owner": owner,
            "task_id": created["room_event"].get("task_id"),
            "closed": any(row.get("role") == "closed" for row in rows),
            "participants": list(members.values()), "messages": messages, "publication": publication,
            "publications": publications,
            "delivery": "next safe point or explicit poll; acknowledgment is not approval"}


def participant(state, value):
    identifier(value, "participant")
    found = next((p for p in state["participants"] if p["participant"] == value and p["joined"]), None)
    if found is None:
        raise ValueError("participant has not joined this room")
    return found


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


def validate_event(rows, event, text):
    """Validate while thread.sh holds its existing file lock; no membership grants authority."""
    if not isinstance(event, dict) or event.get("kind") not in {"created", "join", "bind", "scope", "describe", "leave", "message", "received", "publication"}:
        raise ValueError("invalid room event")
    event_shape(event)
    kind = event["kind"]
    keys = {"created": {"kind", "task_id"},
            "join": {"kind", "participant", "provider", "model", "role", "label", "consumer", "owns", "parent"},
            "bind": {"kind", "participant", "consumer", "previous"},
            "scope": {"kind", "participant", "owns"},
            "describe": {"kind", "participant", "label"},
            "leave": {"kind", "participant"},
            "message": {"kind", "id", "sender", "recipient", "message_kind", "reply_to"},
            "received": {"kind", "participant", "messages"},
            "publication": {"kind", "id", "digest", "status", "delivery_unknown", "turn_id", "retry", "target"}}[kind]
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
    child = os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"
    if child and (not rows or rows[0].get("thread") != os.environ.get("OMS_ROOM_ID")
                  or kind not in {"message", "received"} or
                  event.get("sender", event.get("participant")) != os.environ.get("OMS_ROOM_PARTICIPANT")):
        raise ValueError("a worker can only message or acknowledge as its admitted room participant")
    if kind == "created":
        if len(rows) != 1 or rows[0].get("live") is not True or any(row.get("room_event") for row in rows):
            raise ValueError("room already exists")
        if event.get("task_id"):
            identifier(event["task_id"], "task")
        return True
    state = project(rows)
    if child and participant(state, os.environ.get("OMS_ROOM_PARTICIPANT"))["role"] == "main":
        raise ValueError("a worker cannot speak as a native main")
    if state["closed"]:
        raise ValueError("room is closed")
    if kind == "describe":
        member = participant(state, event["participant"])
        if member["role"] != "main":
            raise ValueError("describe requires an enrolled main")
        bound_room = os.environ.get("OMS_ROOM_ID")
        bound_participant = os.environ.get("OMS_ROOM_PARTICIPANT")
        if ((bound_room or bound_participant)
                and (bound_room != rows[0].get("thread") or bound_participant != event["participant"])):
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
        if "OMS_ROOM_EXPECTED_MAIN_LEAVE" in os.environ:
            from terminal_panel import validate_main_leave
            repo = Path(os.environ["OMS_TH_FILE"]).parents[2]
            validate_main_leave(repo, rows, event, json.loads(os.environ["OMS_ROOM_EXPECTED_MAIN_LEAVE"]))
    elif kind == "message":
        identifier(event.get("id"), "message")
        participant(state, event.get("sender"))
        recipient = event.get("recipient")
        if recipient != "all":
            participant(state, recipient)
        if recipient == event["sender"]:
            raise ValueError("a message needs another participant or all")
        if event.get("message_kind") not in KINDS:
            raise ValueError("invalid message kind")
        if str(event["id"]).startswith("result-") and (
                event["id"] != "result-" + event["sender"] or event["message_kind"] != "handoff"
                or participant(state, event["sender"]).get("parent") != recipient):
            # result-<call> ids are reserved for a call's own handoff to its parent, which settles it on the board.
            raise ValueError("result-<participant> ids are reserved for that participant's handoff to its parent")
        old = next((m for m in state["messages"] if m["id"] == event["id"]), None)
        if not old and len(state["messages"]) >= 1024:
            raise ValueError("room message limit reached; start a new bounded room")
        if old:
            if all(old.get(k) == v for k, v in event.items()) and old["text"] == text:
                return False
            raise ValueError("message id conflicts with its existing content")
        if event.get("reply_to"):
            original = next((m for m in state["messages"] if m["id"] == event["reply_to"]), None)
            if (not original or event["sender"] not in original["targets"]
                    or event["recipient"] != original["sender"]):
                raise ValueError("reply must return to the sender of a message addressed to this participant")
    else:
        participant(state, event.get("participant"))
        mids = event.get("messages")
        if not isinstance(mids, list) or not 1 <= len(mids) <= 200 or any(not isinstance(m, str) for m in mids):
            raise ValueError("ack requires bounded message ids")
        indexed = {m["id"]: m for m in state["messages"]}
        for mid in mids:
            original = indexed.get(mid)
            if not original or event["participant"] not in original["targets"]:
                raise ValueError("cannot acknowledge a message addressed to another participant")
        if all(event["participant"] in indexed[mid]["received_by"] for mid in mids):
            return False
    return True


def command(repo, *args, env=None):
    run = subprocess.run(["bash", str(ENTRY), "thread", "--repo", str(repo)] + list(args),
                         capture_output=True, text=True, check=False, timeout=15, stdin=subprocess.DEVNULL, env=env)
    if run.returncode:
        raise ValueError(clean(run.stderr or run.stdout, 300))
    return run.stdout.strip()


def append(repo, room, event, text, expected_main_leave=None):
    env = os.environ.copy()
    env.pop("OMS_ROOM_EXPECTED_MAIN_LEAVE", None)
    if expected_main_leave is not None:
        if event.get("kind") != "leave":
            raise ValueError("main leave observation requires a leave event")
        env["OMS_ROOM_EXPECTED_MAIN_LEAVE"] = json.dumps(expected_main_leave, allow_nan=False)
    return command(repo, "append", "--id", identifier(room, "room"), "--role", "note", "--text", text,
                   "--room-event", json.dumps(event, ensure_ascii=False), env=env)


def create(repo, room=None, title="Shared work", task_id=None):
    room = identifier(room or "room-" + uuid.uuid4().hex[:12], "room")
    if task_id:
        identifier(task_id, "task")
    if not title.strip() or len(title.encode("utf-8")) > 1000:
        raise ValueError("room title needs 1..1000 bytes")
    validate_event([{"live": True, "thread": room}], {"kind": "created", **({"task_id": task_id} if task_id else {})}, title)
    if (Path(repo) / ".oms/threads" / (room + ".jsonl")).exists():
        raise ValueError("room id already exists; join it explicitly")
    command(repo, "new", "--id", room, "--live", "--topic", title)
    append(repo, room, {"kind": "created", **({"task_id": task_id} if task_id else {})}, title)
    return room


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
    state = state or project(records(repo, room))
    seen, sent = {m["id"] for m in state["messages"]}, []
    attempts = 0
    if len(state["messages"]) >= 1024:
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


def send(repo, room, who, recipient, text, kind="note", reply_to=None, message_id=None, linked=True):
    if not isinstance(text, str) or not text.strip() or len(text.encode("utf-8")) > 4000:
        raise ValueError("message needs 1..4000 UTF-8 bytes")
    if kind == "answer" and not reply_to and linked:
        # An answer without --reply-to never closes its question; link the one it can only mean.
        pending = [q["id"] for q in open_questions(project(records(repo, room)), recipient=who)
                   if q["sender"] == recipient]
        if len(pending) > 1:
            raise ValueError("%s has %d open questions to you; choose one with --reply-to: %s" % (
                recipient, len(pending), ", ".join(pending[:8])))
        if pending:
            reply_to = pending[0]
            print("room: linked this answer to open question %s" % reply_to, file=sys.stderr)
    event = {"kind": "message", "id": message_id or "msg-" + uuid.uuid4().hex,
             "sender": who, "recipient": recipient, "message_kind": kind}
    if reply_to:
        event["reply_to"] = reply_to
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
    event = row.get("room_event", {})
    return (event.get("kind") == "message" and event.get("message_kind") != "status"
            and row.get("seq", 0) > joined_seq and event.get("sender") != who
            and event.get("recipient") in ("all", who))


def updates(repo, room, who, after="", budget=8192, limit=50, *, state=None):
    if (type(budget) is not int or not 1 <= budget <= thread_live.MAX_ROW
            or type(limit) is not int or not 1 <= limit <= 200):
        raise ValueError("updates requires bytes 1..65536 and turns 1..200")
    state = state if state is not None else project(records(repo, room))
    if state["closed"]:
        raise ValueError("room is closed")
    member = participant(state, who)
    if after:
        delta = thread_live.updates(repo, room, after, budget, limit, allow_first_row_over_budget=True)
        return dict(delta, turns=[r for r in delta["turns"] if visible(r, who, member["seq"])])

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


def acknowledge(repo, room, who, mids):
    append(repo, room, {"kind": "received", "participant": who, "messages": mids}, "Messages consumed")


def selected(repo, consumer=None, preferred=None, who=None):
    """Explicit binding wins; otherwise match a native session to its latest join."""
    return _selected(repo, consumer, preferred, who)[0]


def _selected(repo, consumer=None, preferred=None, who=None):
    """Retain the selected projection for a hook's delivery and reminder checks."""
    if preferred:
        state = project(records(repo, identifier(preferred, "room")))
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
        if state["closed"]:
            continue
        for member in state["participants"]:
            if member.get("consumer") == consumer and member["joined"]:
                candidates.append((member["joined_at"], member["seq"], state["id"], member["participant"], state))
    return (candidates[0][2:4], candidates[0][4]) if len(candidates) == 1 else ((None, None) if enrolled else None, None)


def messages(repo, room):
    """Every projected message of the room, oldest first; reads nothing else and writes nothing."""
    return project(records(repo, room))["messages"]


CLOSE_PREFIX = "Close requested: "


def status(repo, room):
    state = project(records(repo, room))
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
    # Outstanding close requests outlive the twelve-message window: the person still has to act on them.
    state["close_requests"] = [m for m in state["messages"] if m["message_kind"] == "question" and not m["answered"]
                               and str(m["text"]).startswith(CLOSE_PREFIX)][-32:]
    state["messages"] = state["messages"][-12:]
    return state


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
    parser.add_argument("action", choices=("new", "join", "bind", "scope", "describe", "leave", "send", "updates", "ack", "show", "list", "publish"))
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
    parser.add_argument("--to", default="all")
    parser.add_argument("--text")
    parser.add_argument("--text-file")
    parser.add_argument("--kind", choices=sorted(KINDS), default="note")
    parser.add_argument("--reply-to")
    parser.add_argument("--message-id")
    parser.add_argument("--after", default="")
    parser.add_argument("--message", action="append", help="consumed message id for ack")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--retry", action="store_true", help="publish only: explicitly retry an uncertain snapshot; may duplicate output")
    parser.add_argument("--app", choices=("codex", "claude"), help="publish target; default Codex fixed receiver")
    parser.add_argument("--allow-wakeup", action="store_true", help="Claude publish only: authorize a native inbox post which can spend model usage")
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    args = parser.parse_args(raw_argv)
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
    if args.action in {"join", "bind", "scope", "describe", "leave", "send", "updates", "ack"} and not args.participant:
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
    try:
        repo = state_repository(args.repo)
        if args.action == "new":
            if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
                raise ValueError("a worker cannot create a room")
            result = {"id": create(repo, args.id, args.topic, args.task_id)}
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
        elif args.action == "send":
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
        elif args.action == "list":
            states, incomplete = discover(repo)
            found = [dict({k: state[k] for k in ("id", "title", "owner")}, message_count=len(state["messages"]))
                     for state in states if not state["closed"]]
            result = {"rooms": found, "scan_limit": MAX_ROOMS, "incomplete": incomplete}
        else:
            result = status(repo, args.id)
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
