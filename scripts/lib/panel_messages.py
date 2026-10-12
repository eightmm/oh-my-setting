"""Messages reader for the panel board: the room's message log, read-only.

Message text is untrusted peer data: it passes through readable()/clean()/wrapped(), never raw, and
reading never acknowledges (no room writes).
"""

import calendar
import time
import unicodedata

from dashboard_projection import clean, display_width, listing, mapping
from panel_view import clipped, wrapped

MAX_MESSAGES, MAX_TEXT = 200, 8000
KINDS = ("question", "answer", "handoff", "status", "note", "decision")


def signature(report):
    """Changes whenever a message arrives or is consumed; the full log is re-read only then."""
    room = mapping(report.get("room"))
    return (room.get("id"), room.get("message_count"), room.get("pending_count"), room.get("received_count"))


def read(repo, room_id):
    """Background read: the last messages of the full projected log."""
    import room
    return room.messages(repo, room_id)[-MAX_MESSAGES:]


def stamp(value, now=None):
    """Local HH:MM, with the date when the message is from another day."""
    try:
        moment = time.localtime(calendar.timegm(time.strptime(str(value), "%Y-%m-%dT%H:%M:%SZ")))
    except ValueError:
        return "--:--"
    today = time.localtime(now)
    return time.strftime("%H:%M" if moment[:3] == today[:3] else "%m-%d %H:%M", moment)


def _people(report, navigation):
    """(name_of, mains, me, members) from the board's own names."""
    from room_view import main_names, nodes, who
    members = nodes(report)
    labels = main_names(members)
    index = {m["participant"]: m for m in members}
    mains = [m["participant"] for m in members if m.get("role") == "main"]
    owners = {key: m["participant"] for m in members if m.get("role") == "main"
              for key in (m["participant"], m.get("attempt")) if key}
    me = owners.get(navigation.get("main_attempt"))
    def name_of(ident):
        if ident == "all":
            return "everyone"
        if ident not in index:
            return clean(ident, 40) or "unknown"
        # Every name carries its role, so a Sol worker never reads like the Sol main.
        return clean(who(index[ident], labels, task=True), 100)
    return name_of, mains, me, members


def _rows(messages, name_of, now=None, members=(), readers=(), width=100, unicode=True, mains=()):
    index = {m["participant"]: m for m in members}
    rows = []
    for raw in messages:
        m = mapping(raw)
        text = readable_text(m.get("text"))
        kind = clean(m.get("message_kind"), 20) or "note"
        pending = bool(set(listing(m.get("pending_for"))) & set(readers))
        targets = listing(m.get("targets"))
        sender = m.get("sender")
        recipient = m.get("recipient") or (targets[0] if len(targets) == 1 else "all")
        if len(mains) == 1 and sender in mains:
            route = name_of(recipient)
        elif len(mains) == 1 and recipient in mains:
            route = name_of(sender)
        else:
            route = name_of(sender) + (" → " if unicode else " -> ") + name_of(recipient)
        first = next((line for line in text.splitlines() if line.strip()), "")
        # The mark is the read state: filled for unread, hollow once read.
        mark = ("●" if pending else "○") if unicode else ("*" if pending else "-")
        prefix = "%s %s %s " % (stamp(m.get("ts"), now)[-5:], mark, kind)
        tail = ""
        # Narrow rows spend their space on the note before showing its route.
        sep = " · " if unicode else " / "
        tail = tail.replace(" · ", sep)
        route = route.replace(" · ", sep)
        first = clean(first, 300) or "(empty)"
        budget = max(0, width - 2)
        tail = clipped(tail, budget)
        prefix = clipped(prefix, max(0, budget - display_width(tail)))
        available = max(0, budget - display_width(prefix + tail))
        route_width = max(0, available - min(display_width(first), max(6, available // 2)) - display_width(sep))
        route = clipped(route, route_width) if route_width >= 3 else ""
        first = clipped(first, max(0, available - display_width(route) - (display_width(sep) if route else 0)))
        content = (first + sep + route if width < 100 else route + sep + first) if route else first
        line = prefix + content + tail
        member = index.get(sender)
        rows.append({"id": str(m.get("id")), "line": line, "text": text, "message": m,
                     "unread": pending, "sender": sender, "first": clean(first, 300),
                     "call": ("result", sender) if member and member.get("role") != "main" else None,
                     "people": {sender, m.get("recipient"), *targets}})
    return rows


def routine(message, room, index, own_notes=True):
    from room_view import result_handoff
    sender = index.get(message.get("sender"), {})
    recipient = index.get(message.get("recipient"), {})
    targets = listing(message.get("targets"))
    return (result_handoff(room, message)
            or sender.get("role") == "main" and recipient and recipient.get("role") != "main"
            and message.get("message_kind") == "handoff"
            and str(message.get("text") or "").startswith("Assigned:")
            or sender.get("role") == "main" and message.get("message_kind") == "status"
            and (message.get("recipient") == "all" or len(targets) > 1)
            or own_notes and sender.get("role") != "main" and sender.get("parent")
            and message.get("message_kind") == "note"
            and (message.get("recipient") == sender["parent"] or targets == [sender["parent"]]))


def call_rows(rows, members, name_of, room, width, unicode=True):
    from room_view import said
    index = {m["participant"]: m for m in members}
    output = []
    for member in members:
        if member.get("role") == "main":
            continue
        # Broadcasts cannot identify a call: their shared message id would collide across call rows.
        messages = [r for r in rows if r["sender"] == member["participant"]
                    or index.get(r["sender"], {}).get("role") == "main"
                    and (r["message"].get("recipient") == member["participant"]
                         or listing(r["message"].get("targets")) == [member["participant"]])]
        notes = [r for r in messages if not routine(r["message"], room, index, own_notes=False)]
        row = dict(messages[-1]) if messages else {"id": None, "text": "", "unread": False,
                                                   "sender": member["participant"], "people": {member["participant"]}}
        row["text"] = notes[-1]["text"] if notes else ""
        row["call"] = ("result", member["participant"]) if messages else None
        row["unread"] = any(r["unread"] for r in messages)
        tail = (" · +%s" % (len(notes) - 1) if len(notes) > 1 else "") + (" · unread" if row["unread"] else "")
        state = " · " + clean(said(member.get("state")))
        budget = max(0, width - 2)
        tail = clipped(tail, budget)
        available = max(0, budget - display_width(tail))
        note = clean(row["text"], 300) or "(no notes)"
        note_width = min(display_width(note), available // 3) if available >= display_width(state) + 6 else 0
        head_width = max(0, available - note_width - (3 if note_width else 0))
        head = (clipped(name_of(member["participant"]), head_width - display_width(state)) + state
                if head_width >= display_width(state) + 3 else clipped(name_of(member["participant"]) + state, head_width))
        row["line"] = head + (" · " + clipped(note, note_width) if note_width else "") + tail
        if not unicode:
            row["line"] = row["line"].replace(" · ", " / ")
        output.append(row)
    return output


def readable_text(text):
    from room_view import readable
    from panel_view import _result_blocks
    output, prose = [], []

    def flush():
        if prose:
            output.append(readable("\n".join(prose), "\n", True))
            prose[:] = []

    for line, kind in _result_blocks(str(text or "")[:MAX_TEXT]):
        if kind == "prose":
            prose.append(line)
        else:
            flush()
            # Keep layout and fence metadata while making terminal controls inert.
            output.append("".join("?" if char != "\t" and unicodedata.category(char)[0] == "C" else char
                                  for char in line))
    flush()
    return "\n".join(output).strip("\n")


def tab_body(report, width, cap, navigation, unicode=True):
    """Rows (text, action, selected) for the Messages tab: the filter line, then the log window."""
    state = navigation.setdefault("messages_view", {"selected": None, "open": None, "filter": 0, "scroll": 0})
    room = mapping(report.get("room"))
    name_of, mains, me, members = _people(report, navigation)
    log = state.get("log")
    loading = log is None and bool(state.get("reading")) and (room.get("message_count") or 0) > len(listing(room.get("messages")))
    messages = (log if log is not None else listing(room.get("messages")))[-MAX_MESSAGES:]
    from room_view import live_readers, settler
    index = {m["participant"]: m for m in members}
    settled, _ = settler(report)
    readers = live_readers(report, members, settled)
    selected = (navigation.get("selected") or (None, None))[1]
    call = index.get(selected)
    filters = [("needs you", None), ("by call", None), ("all", None)]
    if call and call.get("role") != "main":
        filters.append(("this call", selected))
    if len(mains) > 1 and me:
        filters.append(("to/from me", me))
    state["filter"] = state.get("filter", 0) % len(filters)
    label, person = filters[state["filter"]]
    rows = _rows(messages, name_of, members=members, readers=readers, width=width, unicode=unicode, mains=mains)
    if label == "needs you":
        rows = [r for r in rows if not routine(r["message"], room, index)]
    elif label == "by call":
        rows = call_rows(rows, members, name_of, room, width, unicode)
    elif person:
        rows = [r for r in rows if person in r["people"]]
    navigation["message_calls"] = {r["id"]: r["call"] for r in rows if r["id"] is not None}
    ids = [r["id"] for r in rows if r["id"] is not None]
    navigation["message_ids"] = ids
    if ids and (state.get("selected") not in ids or state.get("tail", True) and state["selected"] != ids[-1]):
        state["selected"] = ids[-1]  # the newest message stays selected until the reader moves away
        state["reveal"] = True
    if state.get("open") not in ids:
        state["open"] = None
    navigation["message_has_call"] = bool(navigation["message_calls"].get(state.get("selected")))
    error = clean(state.get("error"), 120)
    budget = max(1, cap - 1 - bool(error))
    state["viewport"] = budget
    body, starts = [], {}
    for r in rows:
        if r["id"] is not None:
            starts[r["id"]] = len(body)
        mark = ("▾ " if unicode else "v ") if r["id"] is not None and r["id"] == state["open"] else \
               ("▸ " if unicode else "> ") if r["id"] is not None and r["id"] == state["selected"] else "  "
        body.append((r["id"], mark + r["line"], True))
        if r["id"] is not None and r["id"] == state["open"]:
            # Each wrapped line consumes at least one of the bounded message characters.
            full = [part for paragraph in r["text"].splitlines()
                    for part in (wrapped(paragraph, width - 4, MAX_TEXT, MAX_TEXT) if paragraph.strip() else [""])]
            body += [(r["id"], "    " + part, False) for part in full]
    if state.pop("reveal", False) and state["selected"] in starts:
        first = starts[state["selected"]]
        if state["selected"] == state["open"] or first < state["scroll"]:
            state["scroll"] = first
        elif first >= state["scroll"] + budget:
            state["scroll"] = first - budget + 1
    scroll = state["scroll"] = min(max(0, state["scroll"]), max(0, len(body) - budget))
    head = "Filter: %s (%d)%s%s" % (label, len(rows), "  · loading earlier messages..." if loading else "",
                                    "  · %d-%d of %d" % (scroll + 1, min(len(body), scroll + budget), len(body))
                                    if len(body) > budget else "")
    out = [(head, None, False)]
    if error:
        out.append(("! " + error, None, False))
    if not rows:
        out.append(("Reading messages..." if loading else "No messages need you" if label == "needs you" else "No messages in this view", None, False))
    out += [(text, ("message", ident) if header and ident is not None else None,
             header and ident is not None and ident == state["selected"])
            for ident, text, header in body[scroll:scroll + budget]]
    return [(clipped(text, width), action, selected) for text, action, selected in out[:cap]]


def clicked(event, navigation):
    """The message id under a click on a Messages tab row, else None."""
    if event[0] != "click":
        return None
    hit = next((h for h in navigation.get("hits", []) if h["y"] == event[2] and h["x1"] <= event[1] <= h["x2"]), None)
    return hit["action"][1] if hit and hit["action"][0] == "message" else None


def handle(event, navigation):
    """Messages-tab keys; True when the event belongs to the tab."""
    state = navigation.setdefault("messages_view", {"selected": None, "open": None, "filter": 0, "scroll": 0})
    ids, kind = navigation.get("message_ids", []), event[0]
    step = max(1, state.get("viewport", 1))
    if kind in {"up", "down"} and ids:
        at = ids.index(state["selected"]) if state.get("selected") in ids else len(ids) - 1
        state["selected"] = ids[min(len(ids) - 1, max(0, at + (1 if kind == "down" else -1)))]
        state["reveal"] = True
    elif kind in {"home", "end"} and ids:
        state["selected"] = ids[0 if kind == "home" else -1]
        state["reveal"] = True
    elif kind in {"left", "right"}:
        state["filter"] = state.get("filter", 0) + (1 if kind == "right" else -1)
        state["selected"] = state["open"] = None
    elif kind == "escape" and state.get("open"):
        state["open"] = None
        state["reveal"] = True
        return True
    elif kind == "f" or kind == "enter" and state.get("selected") in ids and state.get("open") == state["selected"]:
        target = navigation.get("message_calls", {}).get(state.get("selected"))
        if target:
            navigation["open_call"] = target
        else:
            navigation["notice"] = "This message has no call to open"
    elif kind == "enter" and state.get("selected") in ids:
        state["open"] = state["selected"]
        state["reveal"] = True
    elif kind == "click":
        ident = clicked(event, navigation)
        if ident:
            state["selected"] = ident
            state["open"] = None if state.get("open") == ident else ident
            state["reveal"] = True
    elif kind in {"pageup", "pagedown", "scroll"}:
        delta = event[1] if kind == "scroll" else step if kind == "pagedown" else -step
        state["scroll"] = max(0, state.get("scroll", 0) + delta)
    navigation["message_has_call"] = bool(navigation.get("message_calls", {}).get(state.get("selected")))
    state["tail"] = not ids or state.get("selected") == ids[-1]
    return kind in {"up", "down", "enter", "click", "home", "end", "scroll", "pageup", "pagedown", "left", "right", "f"}
