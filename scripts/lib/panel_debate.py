"""Debate reader for the panel board: positions, agreement and decision from recorded evidence.

Debate text is untrusted peer data: every line passes through clean()/wrapped(), never raw.
"""

import re

from dashboard_projection import clean, fit, listing, mapping
from panel_view import MODEL_NAMES, PROVIDER_NAMES, TERMINAL_STATES, wrapped

MAX_DEBATES, MAX_SYNTHESIS = 10, 64 * 1024
DISAGREE = re.compile(r"disagree|conflict|dissent", re.I)
AGREE = re.compile(r"agree|consensus|common", re.I)
HEADING = re.compile(r"^\s{0,3}(?:#{1,6}\s+(.+?)(?:\s+#+)?|\*\*(.+?)\*\*:?)\s*$")


def debates(report, room=None):
    """This room's council debates, newest first; a debate with no known main is not listed."""
    from room_view import nodes
    room = mapping(room or report.get("room"))
    attempts = mapping(report.get("attempts"))
    mains = {m.get("attempt"): m["participant"] for m in nodes(report) if m.get("role") == "main" and m.get("attempt")}
    seen, found = set(), []
    for row in listing(attempts.get("active_recent")) + listing(attempts.get("recent")):
        meta = mapping(row.get("panel"))
        if row.get("tool") != "panel-council" or row.get("attempt_id") in seen:
            continue
        if meta.get("room_id") and meta["room_id"] != room.get("id"):
            continue
        seen.add(row.get("attempt_id"))
        main = mains.get(row.get("parent_attempt_id")) or meta.get("room_participant")
        if row.get("task_id") and main:
            found.append({"task": row["task_id"], "main": main, "state": clean(row.get("state"), 40) or "unknown",
                          "title": clean(meta.get("label"), 120) or row["task_id"],
                          "time": clean(row.get("updated_at"), 40) or ""})
    found.sort(key=lambda d: d["time"], reverse=True)
    return found[:MAX_DEBATES]


def read(repo, target, results_for):
    """Background read: the recorded results plus the bounded synthesis text."""
    report = results_for(repo, target)
    synthesis = next((c for row in report.get("rows", []) for c in row.get("calls", [])
                      if c.get("kind") == "ask-synthesis" and c.get("artifact")), None)
    if synthesis:
        from panel_results import _read
        try:
            text = _read(repo, synthesis["artifact"], MAX_SYNTHESIS).decode("utf-8", "replace")
        except (OSError, ValueError):
            text = ""
        report["synthesis"] = {"path": clean(synthesis["artifact"], 200), "text": text}
    return report


def _provider(call):
    name = (call.get("artifact") or "").rsplit("/", 1)[-1]
    model = call.get("selected_model") or ""
    return next((p for p in PROVIDER_NAMES if name.startswith(p + "-")),
                "claude" if model.startswith("claude") else "codex" if model.startswith("gpt") else "unknown")


def _sentence(text):
    """First sentence: ends at . ! ? followed by whitespace or the end, never inside `code` or "quotes"."""
    closing = None
    for i, ch in enumerate(text):
        if closing:
            closing = None if ch == closing else closing
        elif ch in "`\"“":
            closing = "”" if ch == "“" else ch
        elif ch in ".!?" and (i + 1 == len(text) or text[i + 1].isspace()):
            return text[:i + 1]
    return text


CHOICE = (re.compile(r"(?<![\w`])\(([a-zA-Z])\)"),
          re.compile(r"^[\s*_\"'`>#-]*([A-D])(?:[:.,)/]|\s+[—–-]\s|$)"),
          re.compile(r"\b(?:[Oo]ption|[Cc]hoice|[Aa]nswer)\s+([A-D])\b"),
          re.compile(r"(?i)^[\s*_\"'`>#-]*(yes|no|proceed|revise)\b"))


def choice(text):
    """The leading choice token of a stance - (a), A, yes/no, proceed/revise - or None."""
    for n, pattern in enumerate(CHOICE):
        found = pattern.search(text or "")
        if found:
            return ("(%s)" if n < 3 else "%s") % found[1].lower()
    return None


def _stance(answer):
    """(verdict or None, stance text): the VERDICT line, else the Answer section's first sentence."""
    from peer_artifacts import debate_sections
    verdict = re.search(r"(?im)^[\s*#>_-]*VERDICT\b[\s*_]*[:\-]?[\s*_]*(.+)$", answer)
    if verdict:
        text = clean(verdict[1].strip("*_ "), 400)
        return text, text
    _, sections = debate_sections(answer)
    body = next((b for h, b in sections if h == "Answer" and b.strip()), None)
    if body is None:
        return None, clean(" ".join(answer.split("\n")[:4]), 400)
    return None, _sentence(clean(body, 800))


def _seats(calls):
    """One seat per model, round 1 and 2 answers kept apart by the artifact's -rN suffix."""
    seats = {}
    for call in calls:
        if call.get("kind") != "ask":
            continue
        round_ = re.search(r"-r(\d+)\.md$", call.get("artifact") or "")
        key = call.get("selected_model") or call.get("artifact") or "unknown"
        seat = seats.setdefault(key, {"model": call.get("selected_model"), "provider": _provider(call), "rounds": {}})
        if call.get("answer"):
            seat["rounds"][int(round_[1]) if round_ else 1] = call["answer"]
    return list(seats.values())


def _sections(text):
    """Agreement/Disagreement bodies from the synthesis' own headings; the prompt echo never counts."""
    found, current, skip = {}, None, False
    for line in text.splitlines():
        if line.startswith("## "):
            title = line[3:].strip().lower()
            skip = title.startswith(("prompt", "conversation so far", "exit"))
            current = None
        match = None if skip else HEADING.match(line)
        if match:
            title = (match[1] or match[2]).strip()
            kind = ("Disagreement" if DISAGREE.search(title) else "Agreement" if AGREE.search(title) else None)
            current = kind if kind and kind not in found else None
            if current:
                found[current] = []
        elif current and not skip and line.strip():
            found[current].append(line)
    return {k: clean(" ".join(v), 600) for k, v in found.items() if v}


def _prompt(text):
    """The question as asked: the synthesis artifact's Prompt section, first lines only."""
    lines, inside = [], False
    for line in text.splitlines():
        if line.startswith("## "):
            if inside:
                break
            inside = line[3:].strip().lower() == "prompt"
        elif inside and line.strip():
            lines.append(line.strip())
    return clean(" ".join(lines[:12]), 600)


def _view(navigation):
    return navigation.setdefault("debate_view", {"index": 0, "shown": None, "seat": None, "cursor": 0, "scroll": 0})


def signature(report, main):
    """What the Debate tab marks as new: the shown main's debates and their states."""
    return tuple((d["task"], d["state"]) for d in debates(report) if d["main"] == main)


def _paragraphs(text, width, limit):
    return [part for paragraph in text.splitlines()
            for part in (wrapped(paragraph, width, 50, 8000) if paragraph.strip() else [""])][:limit]


def tab_body(report, main, width, cap, navigation, labels, unicode=True):
    """Rows (text, action, selected) for the Debate tab of one main: the newest debate, or one seat's full answer."""
    from room_view import clock_stamp, readable
    state = _view(navigation)
    entries = [d for d in debates(report) if d["main"] == main]
    name = clean(labels.get(main) or main, 60) if main else "this main"
    if not entries:
        state.update(shown=None, seat=None)
        return [("No debate opened by %s · a main opens one with oms panel --council" % name, None, False)]
    state["index"] = min(max(0, state.get("index", 0)), len(entries) - 1)
    entry = entries[state["index"]]
    key = (entry["task"], entry["main"])
    if state.get("shown") != key:
        for stale in ("report", "error", "refresh"):
            state.pop(stale, None)
        state.update(shown=key, seat=None, cursor=0, scroll=0)
    navigation["debate_targets"] = [(e["task"], e["main"]) for e in entries]
    report_ = state.get("report")
    if report_ is None:
        return [(clean(entry["title"], 120), None, False),
                (clean("! " + state["error"], 120) if state.get("error") else "Reading debate...", None, False)]
    row = next(iter(listing(report_.get("rows"))), {})
    seats = _seats(listing(row.get("calls")))
    state.update(seats=len(seats), viewport=cap)
    dash, arrow = ("—", "→") if unicode else ("-", "->")
    if state.get("seat") is not None and state["seat"] < len(seats):
        seat = seats[state["seat"]]
        latest = max(seat["rounds"]) if seat["rounds"] else 0
        title = "%s %s round %s answer · Esc back" % (
            MODEL_NAMES.get(seat["model"]) or clean(seat["model"], 40) or "Unknown model", dash, latest)
        text = readable(seat["rounds"].get(latest, "No answer recorded"), "\n", True)
        body = [(title, None, False)] + [(line, None, False) for line in _paragraphs(text, width, 400)]
        state["scroll"] = min(max(0, state.get("scroll", 0)), max(0, len(body) - cap))
        return body[state["scroll"]:state["scroll"] + cap]
    answered = sum(1 for seat in seats if seat["rounds"])
    rounds = max([r for seat in seats for r in seat["rounds"]] or [1])
    done = entry["state"] in TERMINAL_STATES
    stamp = clock_stamp(entry["time"])
    head = "%s · opened by %s%s · %d of %d seats answered · %s" % (
        entry["title"], name, " · " + stamp if stamp else "", answered, len(seats),
        "round %d of %d" % (rounds, rounds) if done else "running")
    if len(entries) > 1:
        head += " · debate %d of %d (%s)" % (state["index"] + 1, len(entries), "←→" if unicode else "<>")
    synthesis = mapping(report_.get("synthesis"))
    asked = _prompt(synthesis.get("text") or "") or clean(row.get("title") or entry["title"], 300)
    body = [(part, None, False) for part in wrapped(head, width, 2)]
    for n, part in enumerate(wrapped(asked, width - len("Question: "), 3)):
        body.append((("Question: " if not n else " " * len("Question: ")) + part, None, False))
    state["cursor"] = min(max(0, state.get("cursor", 0)), max(0, len(seats) - 1))
    starts = {}
    mark = "▸ " if unicode else "> "
    for i, seat in enumerate(seats):
        model = MODEL_NAMES.get(seat["model"]) or clean(seat["model"], 40) or "Unknown model"
        if seat["rounds"]:
            first, last = seat["rounds"][min(seat["rounds"])], seat["rounds"][max(seat["rounds"])]
            stance = fit(clean(_stance(last)[1], 400) or "No answer recorded", 140)
            note = ""
            if len(seat["rounds"]) > 1:
                before, after = choice(_stance(first)[1]), choice(_stance(last)[1])
                if before and after and before != after:
                    note = " (changed in round %d: %s %s %s)" % (max(seat["rounds"]), before, arrow, after)
            text = "%s %s %s%s" % (model, dash, stance, note)
        else:
            text = "%s %s no answer recorded yet" % (model, dash)
        starts[i] = len(body)
        for n, part in enumerate(wrapped(text, width - 2, 2)):
            body.append((((mark if i == state["cursor"] else "  ") if not n else "  ") + part,
                         ("seat", i), i == state["cursor"]))
    if not seats:
        body.append(("No seat answers recorded yet", None, False))
    for title, text in _sections(synthesis.get("text") or "").items():
        for n, part in enumerate(wrapped(title + ": " + text, width, 2)):
            body.append((part, None, False))
    summary = clean(row.get("summary"), 800) or ""
    outcome = ("(outcome: %s)" % clean(row.get("outcome"), 40)) if row.get("outcome") else ""
    decision = (summary + " " + outcome).strip() or "No decision recorded yet"
    body += [(part, None, False) for part in wrapped("Decision: " + decision, width, 3)]
    if state.pop("reveal", False) and state["cursor"] in starts:
        first_row = starts[state["cursor"]]
        if first_row < state["scroll"]:
            state["scroll"] = first_row
        elif first_row >= state["scroll"] + cap:
            state["scroll"] = first_row - cap + 1
    state["scroll"] = min(max(0, state.get("scroll", 0)), max(0, len(body) - cap))
    return body[state["scroll"]:state["scroll"] + cap]


def handle(event, navigation):
    """Debate-tab keys; True when the event belongs to the tab. Opening a seat resets its scroll."""
    state, kind = _view(navigation), event[0]
    targets = navigation.get("debate_targets", [])
    step = max(1, state.get("viewport", 1))
    if state.get("seat") is not None:
        if kind == "escape":
            state.update(seat=None, scroll=0, reveal=True)
            return True
        delta = {"up": -1, "down": 1, "pageup": -step, "pagedown": step}.get(kind)
        delta = event[1] if kind == "scroll" else delta
        if delta is not None:
            state["scroll"] = max(0, state.get("scroll", 0) + delta)
        return delta is not None or kind in {"enter", "left", "right", "home", "end"}
    seats = state.get("seats", 0)
    if kind in {"left", "right"} and len(targets) > 1:
        state["index"] = (state.get("index", 0) + (1 if kind == "right" else -1)) % len(targets)
    elif kind in {"up", "down"} and seats:
        state["cursor"] = min(seats - 1, max(0, state.get("cursor", 0) + (1 if kind == "down" else -1)))
        state["reveal"] = True
    elif kind == "enter" and seats:
        state.update(seat=state.get("cursor", 0), scroll=0)
    elif kind == "click":
        hit = next((h for h in navigation.get("hits", []) if h["y"] == event[2] and h["x1"] <= event[1] <= h["x2"]), None)
        if not hit or hit["action"][0] != "seat":
            return False
        state.update(cursor=hit["action"][1], seat=hit["action"][1], scroll=0)
    elif kind in {"pageup", "pagedown", "scroll"}:
        delta = event[1] if kind == "scroll" else step if kind == "pagedown" else -step
        state["scroll"] = max(0, state.get("scroll", 0) + delta)
    else:
        return False
    return True
