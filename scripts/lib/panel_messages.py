"""Messages reader for the panel board: the room's message log, read-only.

Message text is untrusted peer data: terminal controls are made inert before display,
and reading never acknowledges (no room writes).
"""

import calendar
import time
import unicodedata

from dashboard_projection import clean, display_width, listing, mapping
from panel_view import clipped, wrapped, plan_tail_columns, format_tail_row

MAX_MESSAGES, RETAIN, MAX_TEXT = 200, 2000, 8000
KINDS = ("question", "answer", "handoff", "status", "note", "decision")


def signature(report):
    """Changes whenever a message arrives or is consumed; the full log is re-read only then."""
    room = mapping(report.get("room"))
    return (room.get("id"), room.get("message_count"), room.get("pending_count"), room.get("received_count"))


def read(repo, room_id):
    """Background read: one bounded list, with the full projected count attached."""
    import room
    messages = room.messages(repo, room_id)
    return MessageWindow(messages[-RETAIN:], len(messages))


class MessageWindow(list):
    """List of projected message dicts and the full log count before retention."""

    def __init__(self, messages=(), total_count=None):
        super().__init__(messages)
        self.total_count = len(self) if total_count is None else total_count


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
    from room_view import main_names, nodes, member_identity
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
        return clean(member_identity(index[ident]), 100)
    return name_of, mains, me, members


def _rows(messages, name_of, now=None, members=(), readers=(), width=100, unicode=True, mains=(), successors=None):
    from room_view import message_address, message_pending
    successors = mapping(successors)
    index = {m["participant"]: m for m in members}
    rows = []
    for raw in messages:
        m = mapping(raw)
        text = readable_text(m.get("text"))
        kind = clean(m.get("message_kind"), 20) or "note"
        pending = bool(message_pending(m, successors) & set(readers))
        targets = listing(m.get("targets"))
        sender = m.get("sender")
        recipient = m.get("recipient") or (targets[0] if len(targets) == 1 else "all")
        addressed = message_address(recipient, members, successors, name_of)
        if len(mains) == 1 and sender in mains:
            route = addressed
        elif len(mains) == 1 and recipient in mains and addressed == name_of(recipient):
            route = name_of(sender)
        else:
            route = name_of(sender) + (" → " if unicode else " -> ") + addressed
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
                     "people": {sender, successors.get(recipient, recipient),
                                *(successors.get(t, t) for t in targets)}})
    return rows


def routine(message, room, index, own_notes=True):
    from room_view import result_handoff
    if str(message.get("id") or "").startswith("judgment-"):
        return False  # Automatic review notes ask the parent for admission or a decision.
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


def needs_main(report):
    """Unacknowledged nonroutine questions or addressed mail for a main, counted once per message."""
    from room_view import message_pending, nodes
    room = mapping(report.get("room"))
    index = {m["participant"]: m for m in nodes(report)}
    mains = {ident for ident, m in index.items() if m.get("role") == "main"}
    successors = mapping(report.get("message_successors"))
    messages = map(mapping, listing(room.get("pending_messages")) if "pending_messages" in room else listing(room.get("messages")))
    return [m for m in messages if message_pending(m, successors) & mains
            and not m.get("answered") and not routine(m, room, index)
            and (m.get("message_kind") == "question" or m.get("recipient") != "all"
                 and (successors.get(m.get("recipient"), m.get("recipient")) in mains
                      or {successors.get(t, t) for t in listing(m.get("targets"))} & mains))]


def exited_mail(message, index):
    from panel_view import TERMINAL_STATES
    target = index.get(message.get("recipient"), {})
    return (target.get("role") == "main" and target.get("state") in TERMINAL_STATES
            and message.get("message_kind") not in {"answer", "status"} and not message.get("answered")
            and target.get("participant") in listing(message.get("pending_for")))


def call_rows(rows, members, name_of, room, width, unicode=True, summaries=None):
    from panel_view import status_tag
    index = {m["participant"]: m for m in members}
    summaries = mapping(summaries)
    output = []
    for member in members:
        if member.get("role") == "main":
            continue
        summary = mapping(summaries.get(member["participant"]))
        # A broadcast can belong to its sender's call, but never to every addressed call.
        messages = [r for r in rows if r["sender"] == member["participant"]
                    or index.get(r["sender"], {}).get("role") == "main"
                    and (r["message"].get("recipient") == member["participant"]
                         or listing(r["message"].get("targets")) == [member["participant"]])]
        notes = [r for r in messages if not routine(r["message"], room, index, own_notes=False)]
        row = dict(messages[-1]) if messages else {"id": None, "text": "", "unread": False,
                                                   "sender": member["participant"], "people": {member["participant"]}}
        row["text"] = summary.get("text", notes[-1]["text"] if notes else "")
        row["call"] = ("result", member["participant"]) if messages or summary.get("has_messages") else None
        row["unread"] = any(r["unread"] for r in messages) or bool(summary.get("unread"))
        note_count = summary.get("note_count", len(notes))
        tail = (" · +%s" % (note_count - 1) if note_count > 1 else "") + (" · unread" if row["unread"] else "")
        phase = "timed_out" if member.get("exit") == 124 else member.get("state")
        if member.get("_decided"):
            code = member.get("exit")
            phase = "done" if code == 0 else "failed" if type(code) is int else "unknown"
        state = " " + status_tag(phase, exit_code=member.get("exit"), unicode=unicode)
        state += " [accepted]" if member.get("decision") == "accepted" else " [main decided]" if member.get("_decided") else ""
        note = clipped(clean(row["text"], 300) or "(no notes)", max(0, (width - 2) // 3))
        sep = " · " if unicode else " / "
        row["_tail"] = [(state.strip(), "metadata"), ((sep + note) if note else "", "metadata"),
                        (tail.replace(" · ", sep).strip(), "metadata")]
        row["_label"] = name_of(member["participant"])
        output.append(row)
    plan = plan_tail_columns([row["_tail"] for row in output], max(0, width - 2), optional=(1,))
    for row, values in zip(output, plan["rows"]):
        row["line"], _ = format_tail_row([], (row.pop("_label"), "text"), values, plan)
        row.pop("_tail")
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


def main_mail(state, participant):
    from room_view import result_handoff
    return [m for m in listing(state.get("messages")) if participant in listing(mapping(m).get("targets"))
            and not result_handoff(state, m)]


def mail_page(state, participant, width):
    """The selected main's addressed log, oldest first; only terminal controls and wrapping alter bodies."""
    from room_view import main_names, who
    from panel_view import _result_structured
    width = max(8, width)
    state = dict(state, messages=listing(state.get("messages"))[-MAX_MESSAGES:])
    members = listing(state.get("participants"))
    labels = main_names(members)
    names = {m["participant"]: who(m, labels) for m in members}
    lines = [clipped("Messages to " + clean(names.get(participant, participant)), width)]
    for message in main_mail(state, participant):
        sender = clean(names.get(message.get("sender"), message.get("sender")), 100) or "unknown"
        lines += ["", clipped("From %s / %s" % (sender, stamp(message.get("ts"))), width)]
        for line in str(message.get("text") or "(empty)").split("\n"):
            lines.extend(_result_structured(line, width))
    return "\n".join(lines)


def tab_body(report, width, cap, navigation, unicode=True):
    """Rows (text, action, selected) for the Messages tab: the filter line, then the log window."""
    state = navigation.setdefault("messages_view", {"selected": None, "open": None, "filter": 0, "scroll": 0})
    room = mapping(report.get("room"))
    name_of, mains, me, members = _people(report, navigation)
    log = state.get("log")
    loading = log is None and bool(state.get("reading")) and (room.get("message_count") or 0) > len(listing(room.get("messages")))
    source = log if log is not None else listing(room.get("messages"))
    messages = list(source)[-RETAIN:]
    total_count = getattr(source, "total_count", room.get("message_count", len(source)))
    total_count = max(int(total_count or 0), len(messages))
    from room_view import live_readers, message_pending, settler
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
    successors = mapping(report.get("message_successors"))
    if label == "needs you":
        matching = [message for message in messages
                    if exited_mail(message, index) or not routine(message, room, index)]
    elif label == "by call":
        summaries = {}
        groups = {}
        for order, message in enumerate(messages):
            targets = listing(message.get("targets"))
            recipient = message.get("recipient") or (targets[0] if len(targets) == 1 else "all")
            sender = message.get("sender")
            for member in members:
                participant = member.get("participant")
                if member.get("role") == "main" or not participant:
                    continue
                addressed = (recipient != "all" and len(targets) <= 1
                             and index.get(sender, {}).get("role") == "main"
                             and (recipient == participant or targets == [participant]))
                if sender == participant or addressed:
                    groups.setdefault(participant, []).append((order, message))
                    break
        # Summaries cover the retained window; only one representative per call is formatted.
        representatives = []
        for participant, group in groups.items():
            call_messages = [message for _, message in group]
            notes = [message for message in call_messages
                     if not routine(message, room, index, own_notes=False)]
            summaries[participant] = {
                "has_messages": True,
                "note_count": len(notes),
                "text": readable_text(notes[-1].get("text")) if notes else "",
                "unread": any(message_pending(message, successors) & set(readers) for message in call_messages),
            }
            representatives.append(group[-1])
        matching = [message for _, message in sorted(representatives, key=lambda entry: entry[0])]
    elif label == "all":
        matching = messages
    elif person:
        matching = []
        for message in messages:
            targets = listing(message.get("targets"))
            recipient = message.get("recipient") or (targets[0] if len(targets) == 1 else "all")
            sender = message.get("sender")
            if label == "this call":
                direct = recipient != "all" and len(targets) <= 1
                if sender == person or direct and (recipient == person or targets == [person]):
                    matching.append(message)
            elif (successors.get(sender, sender) == person
                  or successors.get(recipient, recipient) == person
                  or any(successors.get(target, target) == person for target in targets)):
                matching.append(message)
    else:
        matching = []
    match_count = len(matching)
    display_messages = matching[-MAX_MESSAGES:]
    rows = _rows(display_messages, name_of, members=members, readers=readers, width=width,
                 unicode=unicode, mains=mains, successors=successors)
    if label == "by call":
        rows = call_rows(rows, members, name_of, room, width, unicode, summaries=summaries)
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
    truncated = total_count > len(source)
    searching = truncated and len(source) >= RETAIN
    if loading:
        count_text = "room-wide: %s messages" % format(total_count, ",")
    elif label == "all":
        count_text = ("newest %d of %s" % (len(display_messages), format(total_count, ","))
                      if total_count > MAX_MESSAGES else str(total_count))
    elif label == "by call":
        count_text = str(len(rows))
    elif searching:
        count_text = "newest %d in search window" % len(display_messages)
    elif truncated:
        count_text = "at least %d" % match_count
    elif match_count > MAX_MESSAGES:
        count_text = "newest %d of %s" % (len(display_messages), format(match_count, ","))
    else:
        count_text = str(match_count)
    search_note = ("  · searching the newest %s of %s room messages" %
                   (format(RETAIN, ","), format(total_count, ","))) if searching and not loading else ""
    head = "Filter: %s (%s)%s%s%s" % (label, count_text,
                                    "  · loading earlier messages..." if loading else "", search_note,
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
    return [(clipped(text, width), action, selected) for text, action, selected in (out[-1:] if cap == 1 else out[:cap])]


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
    elif kind == "f":
        target = navigation.get("message_calls", {}).get(state.get("selected"))
        if target:
            navigation["open_call"] = target
        else:
            navigation["notice"] = "This message has no call to open"
    elif kind == "enter" and state.get("selected") in ids:
        state["open"] = None if state.get("open") == state["selected"] else state["selected"]
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
