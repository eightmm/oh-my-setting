"""Debate reader for the panel board: positions, agreement and decision from recorded evidence.

Debate text is untrusted peer data: every line passes through clean()/wrapped(), never raw.
"""

import re

from dashboard_projection import clean, listing, mapping
from panel_view import MODEL_NAMES, PALETTE, PROVIDER_NAMES, clipped, wrapped

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
                          "time": (clean(row.get("updated_at"), 40) or "").replace("T", " ")[:16]})
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
    first = re.match(r"(.+?[.!?])(?:\s|$)", clean(body, 800))
    return None, first[1] if first else clean(body, 400)


def _seats(calls):
    """One seat per model, round 1 and 2 answers kept apart by the artifact's -rN suffix."""
    seats = {}
    for call in calls:
        if call.get("kind") != "ask" or not call.get("answer"):
            continue
        round_ = re.search(r"-r(\d+)\.md$", call.get("artifact") or "")
        key = call.get("selected_model") or call.get("artifact") or "unknown"
        seat = seats.setdefault(key, {"model": call.get("selected_model"), "provider": _provider(call), "rounds": {}})
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


def _block(lines, text, width, limit, indent="  "):
    lines += [indent + part for part in wrapped(text, width - len(indent), limit)]


def _body(entry, debate_report, width):
    row = next((r for r in listing(debate_report.get("rows"))), {})
    lines = ["Question"]
    prompt = clean(row.get("title") or entry["title"], 160)
    _block(lines, prompt, width, 1)
    asked = _prompt(mapping(debate_report.get("synthesis")).get("text") or "")
    if asked and asked != prompt:
        _block(lines, asked, width, 4)
    seats = _seats(listing(row.get("calls")))
    for seat in seats:
        name = MODEL_NAMES.get(seat["model"]) or clean(seat["model"], 40) or "Unknown model"
        provider = PROVIDER_NAMES.get(seat["provider"], seat["provider"])
        lines += ["", "%s (%s)" % (name, provider)]
        latest = seat["rounds"][max(seat["rounds"])]
        verdict, stance = _stance(latest)
        _block(lines, stance or "No answer recorded", width, 4)
        if len(seat["rounds"]) > 1:
            first = _stance(seat["rounds"][min(seat["rounds"])])
            if (verdict or stance) != (first[0] or first[1]):
                _block(lines, "Changed after round 1: was " + clean(first[0] or first[1], 200), width, 2)
    if not seats:
        lines += ["", "No seat answers recorded yet"]
    synthesis = mapping(debate_report.get("synthesis"))
    for title, text in _sections(synthesis.get("text") or "").items():
        lines += ["", title]
        _block(lines, text, width, 4)
    lines += ["", "Decision"]
    summary = clean(row.get("summary"), 800)
    if summary or row.get("outcome"):
        _block(lines, ((summary + " " if summary else "") + ("(outcome: %s)" % clean(row.get("outcome"), 40)
                                                           if row.get("outcome") else "")).strip(), width, 4)
    else:
        lines.append("  No decision recorded yet")
    if synthesis.get("path"):
        lines += ["", "Full text: " + synthesis["path"]]
    return lines


def render_debate(report, width, height, navigation, color=False, unicode=True):
    """(lines, hits): the debate list, or one opened debate paged by navigation['offset']."""
    state = navigation.setdefault("debate_view", {"index": 0, "open": None})
    entries = debates(report)
    state["index"] = min(max(0, state.get("index", 0)), max(0, len(entries) - 1))
    navigation["debate_targets"] = [(e["task"], e["main"]) for e in entries]
    room = mapping(report.get("room"))
    head = "OMS / Debates / " + clean(room.get("title") or room.get("id") or "room", 60)
    lines, hits = [clipped(head, width), ""], []
    opened = next((e for e in entries if (e["task"], e["main"]) == state.get("open")), None)
    budget = max(1, height - len(lines) - 1)
    if opened is None:
        state["open"] = None
        if not entries:
            lines.append("No debates in this room yet")
        for i, entry in enumerate(entries[:budget]):
            row = "%s %s - %s - %s - %s" % (">" if i == state["index"] else " ", entry["title"], entry["main"],
                                            entry["state"], entry["time"] or "time unrecorded")
            lines.append(clipped(row, width))
            hits.append({"y": len(lines), "x1": 1, "x2": width, "action": ("debate", (entry["task"], entry["main"]))})
        footer = ("Enter Open  %s Move  d/Esc Graph" % ("↑↓" if unicode else "^v"))
        navigation["viewport"] = budget
    else:
        pending = "report" not in state
        body = ([clipped("Reading debate...", width)] if pending else
                [clipped(line, width) for line in _body(opened, state["report"], width)])
        if state.get("error"):
            body.append(clipped("! " + clean(state["error"], 120), width))
        offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - budget))
        navigation["offset"] = offset
        navigation["viewport"] = budget
        lines.append(clipped("%s - %s - %s" % (opened["title"], opened["main"], opened["state"]), width))
        lines += body[offset:offset + budget - 1]
        footer = "%s%s  PgUp/PgDn Scroll  Esc List  d Graph" % (
            "%d-%d/%d  " % (offset + 1, min(len(body), offset + budget - 1), len(body)) if len(body) > budget - 1 else "",
            "↑↓" if unicode else "^v")
    lines = lines[:height - 1] + [clipped(footer, width)]
    if color:
        lines = [PALETTE["head"] + lines[0] + "\033[0m"] + lines[1:-1] + [PALETTE["dim"] + lines[-1] + "\033[0m"]
    return lines[:height], hits


def handle(event, navigation):
    """Debate-view keys; True when the event belongs to this view. Opening resets the read."""
    state = navigation.setdefault("debate_view", {"index": 0, "open": None})
    targets, kind = navigation.get("debate_targets", []), event[0]

    def open_(target):
        state.update(open=target, index=targets.index(target) if target in targets else 0)
        for key in ("report", "error", "refresh"):
            state.pop(key, None)
        navigation["offset"] = 0

    if state.get("open"):
        step = max(1, navigation.get("viewport", 1))
        delta = {"up": -1, "down": 1, "pageup": -step, "pagedown": step}.get(kind)
        if kind == "scroll":
            delta = event[1]
        if delta is None:
            return kind in {"click", "enter", "home", "end"}
        navigation["offset"] = max(0, navigation.get("offset", 0) + delta)
        return True
    if kind in {"up", "down"} and targets:
        state["index"] = (state.get("index", 0) + (1 if kind == "down" else -1)) % len(targets)
    elif kind == "enter" and targets:
        open_(targets[min(state.get("index", 0), len(targets) - 1)])
    elif kind == "click":
        hit = next((h for h in navigation.get("hits", []) if h["y"] == event[2] and h["x1"] <= event[1] <= h["x2"]), None)
        if hit:
            open_(hit["action"][1])
    return kind in {"up", "down", "enter", "click", "home", "end", "scroll", "pageup", "pagedown", "left", "right"}
