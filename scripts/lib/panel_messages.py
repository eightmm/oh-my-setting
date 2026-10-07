"""Messages reader for the panel board: the room's message log, read-only.

Message text is untrusted peer data: it passes through readable()/clean()/wrapped(), never raw, and
reading never acknowledges (no room writes).
"""

import calendar
import time

from dashboard_projection import clean, listing, mapping
from panel_view import wrapped

MAX_MESSAGES, MAX_TEXT, MAX_BODY_LINES = 200, 8000, 400
KINDS = ("question", "answer", "handoff", "status")


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
    """(name_of, mains, me, selected main) from the board's own names."""
    from room_view import main_names, nodes, who
    members = nodes(report)
    labels = main_names(report, members)
    index = {m["participant"]: m for m in members}
    mains = [m["participant"] for m in members if m.get("role") == "main"]
    owners = {key: m["participant"] for m in members if m.get("role") == "main"
              for key in (m["participant"], m.get("attempt")) if key}
    me = owners.get(navigation.get("main_attempt"))
    chosen = (navigation.get("selected") or (None, None))[1]
    found = index.get(chosen)
    selected = (found["participant"] if found and found.get("role") == "main" else
                owners.get(found.get("parent")) if found else None) or None

    def name_of(ident):
        if ident == "all":
            return "everyone"
        if ident not in index:
            return clean(ident, 40) or "unknown"
        # A numbered main is unique; any other name carries its role, so a Sol worker never reads like the Sol main.
        label = labels.get(ident) or ""
        return clean(label if label.startswith("#") else who(index[ident], labels), 60)
    return name_of, mains, me, selected


def _rows(messages, name_of, now=None):
    rows = []
    for m in messages:
        m = mapping(m)
        text = readable_text(m.get("text"))
        kind = m.get("message_kind")
        pending = bool(listing(m.get("pending_for")))
        targets = listing(m.get("targets"))
        head = "%s  %s → %s" % (stamp(m.get("ts"), now), name_of(m.get("sender")),
                                name_of(m.get("recipient") or (targets[0] if len(targets) == 1 else "all")))
        head += ("  · unread" if pending else "") + ("  · " + kind if kind in KINDS else "")
        first = next((line for line in text.splitlines() if line.strip()), "")
        rows.append({"id": str(m.get("id")), "line": head + "  · " + (clean(first, 300) or "(empty)"),
                     "text": text, "people": {m.get("sender"), m.get("recipient"), *listing(m.get("targets"))}})
    return rows


def readable_text(text):
    from room_view import readable
    return readable(str(text or "")[:MAX_TEXT], "\n", True)


def tab_body(report, width, cap, navigation, unicode=True):
    """Rows (text, action, selected) for the Messages tab: the filter line, then the log window."""
    state = navigation.setdefault("messages_view", {"selected": None, "open": None, "filter": 0, "scroll": 0})
    room = mapping(report.get("room"))
    name_of, mains, me, chosen = _people(report, navigation)
    log = state.get("log")
    loading = log is None and (room.get("message_count") or 0) > len(listing(room.get("messages")))
    messages = (log if log is not None else listing(room.get("messages")))[-MAX_MESSAGES:]
    filters = [("all", None)] + ([("to/from " + name_of(chosen), chosen)] if chosen else []) + \
              ([("to/from me", me)] if me else [])
    state["filter"] = state.get("filter", 0) % len(filters)
    label, person = filters[state["filter"]]
    rows = [r for r in _rows(messages, name_of) if person is None or person in r["people"]]
    ids = [r["id"] for r in rows]
    navigation["message_ids"] = ids
    if ids and (state.get("selected") not in ids or state.get("tail", True) and state["selected"] != ids[-1]):
        state["selected"] = ids[-1]  # the newest message stays selected until the reader moves away
        state["reveal"] = True
    if state.get("open") not in ids:
        state["open"] = None
    budget = max(1, cap - 1)
    state["viewport"] = budget
    body, starts = [], {}
    for r in rows:
        starts[r["id"]] = len(body)
        mark = ("▾ " if unicode else "v ") if r["id"] == state["open"] else \
               ("▸ " if unicode else "> ") if r["id"] == state["selected"] else "  "
        body.append((r["id"], mark + r["line"], True))
        if r["id"] == state["open"]:
            full = [part for paragraph in r["text"].splitlines()
                    for part in (wrapped(paragraph, width - 4, 50, MAX_TEXT) if paragraph.strip() else [""])]
            body += [(r["id"], "    " + part, False) for part in full[:MAX_BODY_LINES]]
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
    if not rows:
        out.append(("Reading messages..." if loading else "No messages in this room yet", None, False))
    out += [(text, ("message", ident) if header else None, header and ident == state["selected"])
            for ident, text, header in body[scroll:scroll + budget]]
    return out[:cap]


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
    state["tail"] = not ids or state.get("selected") == ids[-1]
    return kind in {"up", "down", "enter", "click", "home", "end", "scroll", "pageup", "pagedown", "left", "right"}
