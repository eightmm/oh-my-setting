"""Room graph drawn from participation, addressed messages and lifecycle evidence."""

import calendar
import re
import time

from dashboard_projection import ELLIPSIS, clean, count as valid_count, display_width, fit, listing, mapping
from panel_view import (ATTENTION_STATES, MODEL_NAMES, LIVE_STATES, TERMINAL_STATES, PALETTE, activity, box_edge, box_row,
                        clipped, location_label, status_alerts, worker_rows, wrapped)


def nodes(report):
    room = mapping(report.get("room"))
    attempts = mapping(report.get("attempts"))
    linked = {}
    rows = listing(attempts.get("active_recent")) + listing(attempts.get("recent"))
    rows = sorted(rows, key=lambda row: mapping(row.get("panel")).get("room_id") != room.get("id"))
    for row in rows:
        meta = mapping(row.get("panel"))
        if meta.get("room_id") and meta["room_id"] != room.get("id"):
            continue
        ident = meta.get("room_participant")
        if ident and not meta.get("room_id") and ident != row.get("attempt_id"):
            continue
        if meta.get("role") == "main" and not ident:
            ident = row.get("attempt_id")
        if ident:
            linked.setdefault(ident, []).append(row)
    output = []
    guarded = {m.get("sender") for m in map(mapping, listing(room.get("messages")))
               if result_handoff(room, m) and "worker guard stopped this run" in str(m.get("text")).lower()}
    for member in listing(room.get("participants")):
        if not member.get("joined"):
            continue
        # Dispatched children inherit their main's participant as provenance;
        # only a same-role attempt is the member itself.
        attempt = next((row for row in linked.get(member.get("participant"), [])
                        if mapping(row.get("panel")).get("role") == member.get("role")), {})
        meta = mapping(attempt.get("panel"))
        if not attempt:
            # Outside the projection window: the full-projection row for this call, when the snapshot found one.
            attempt = mapping(mapping(report.get("room_attempts")).get(member.get("participant")))
        status = mapping(mapping(report.get("provider_status")).get("codex"))
        native = mapping(status.get("native_models"))
        reading = mapping(native.get(member.get("participant")))
        live_native = (member.get("role") == "main" and member.get("provider") == "codex"
                       and attempt.get("state") not in TERMINAL_STATES
                       and member.get("consumer"))
        current = (live_native and reading.get("consumer") == member.get("consumer")
                   and reading.get("source") == "native state" and reading.get("model"))
        model = meta.get("model") or member.get("model")
        effort = meta.get("effort")
        if live_native and status.get("native_model_checked"):
            model, effort = (reading.get("model"), reading.get("effort")) if current else (None, None)
        # Attempt evidence wins; a call's result message is the fallback once its attempt left the projection.
        result = mapping(mapping(room.get("call_results")).get(member.get("participant")))
        code = result.get("exit")
        derived = "presence unknown" if member.get("role") == "main" or type(code) is not int else \
            "done" if code == 0 else "failed"
        output.append(dict(member, attempt=attempt.get("attempt_id"), state=attempt.get("state") or derived,
                           ended=result.get("ts"), exit=code if type(code) is int else None,
                           guard=member.get("participant") in guarded,
                           model=model, model_at_launch=meta.get("model") or member.get("model"),
                           task_id=attempt.get("task_id"), location=meta.get("location"),
                           effort=effort, access=meta.get("access"),
                           started_by=mapping(report.get("main_starters")).get(member.get("participant")),
                           title=member.get("label_override") or meta.get("label") or member.get("label")))
    # A continued worker shares its task with the earlier rounds of the same main.
    rounds = {}
    for member in sorted(output, key=lambda m: m.get("seq", 0)):
        if member.get("role") == "worker" and member.get("task_id") and member.get("parent"):
            earlier = rounds.setdefault((member["parent"], member["task_id"]), [])
            member.update(round=len(earlier) + 1, continues=earlier[-1] if earlier else None)
            earlier.append(member["participant"])
    rank = lambda member: (0 if member["state"] in LIVE_STATES else 2 if member["state"] in {"done", "cancelled"} else 1, -member.get("seq", 0))
    return sorted(output, key=rank)


def spawn_bar(width, navigation):
    """The board's fixed last row as (text, [(x1, x2, provider)]); None where the board has no live input."""
    installed = navigation.get("installed")
    if installed is None:
        return None
    if not navigation.get("spawn_managed"):
        return "Start a main: needs the tmux panel (oms panel)", []
    if navigation.get("spawn_armed"):
        return "Start a main: 1 Codex  2 Claude  Esc cancel", []
    if navigation.get("spawn_busy"):
        return "Starting a main...", []
    if not installed:
        return "No Codex or Claude CLI is installed to start a main", []
    for suffix in (" main", ""):
        labels = ["[+ %s%s]" % (name.capitalize(), suffix) for name in installed]
        if len("  ".join(labels)) <= width:
            break
    x, spots = 0, []
    for name, label in zip(installed, labels):
        if x < width:
            spots.append((x + 1, min(width, x + len(label)), name))
        x += len(label) + 2
    return "  ".join(labels), spots


def window_order(report, mains):
    """Mains in tmux window order, the order F6/F7 step through; join order where no window is known."""
    windows = mapping(report.get("main_windows"))
    slot = lambda m: windows.get(m["participant"]) if type(windows.get(m["participant"])) is int else float("inf")
    return sorted(mains, key=lambda m: (slot(m), m.get("seq", 0), m["participant"]))


def model_name(member):
    return MODEL_NAMES.get(member.get("model"), member.get("model")) or member.get("provider", "unknown")


def open_count(room, participant):
    """Questions to this participant that have no linked answer yet."""
    return sum(mapping(q).get("recipient") == participant for q in listing(mapping(room).get("open_questions")))


def context_note(report, member):
    """(percent, 'dim'|None|'alert') of the context left for this main, from the reading the board already collected."""
    if member.get("role") != "main":
        return None
    row = mapping(mapping(report.get("provider_status")).get(member.get("provider")))
    if row.get("participant") != member.get("participant"):
        return None
    from panel_metrics import percent
    left = percent(row.get("context_left"))
    return None if left is None else (left, "dim" if left > 30 else "alert" if left <= 15 else None)


def main_names(report, members):
    """Display name per main: its tmux window number first, the F6/F7 and Ctrl-b N order, then the model.

    Without a known window, a model another main shares gets the id tail instead."""
    mains = [m for m in members if m.get("role") == "main"]
    windows = mapping(report.get("main_windows"))
    plain = [model_name(m) for m in mains]
    names = {}
    for m, name in zip(mains, plain):
        slot = windows.get(m["participant"])
        names[m["participant"]] = ("#%s %s" % (slot, name) if type(slot) is int else
                                   name if plain.count(name) == 1 else "%s #%s" % (name, m["participant"][-4:]))
    return names


def main_links(room, mains):
    """One link per unordered pair of mains that exchanged mail, left main first in window order.

    Unread pairs lead, then the most recent exchange."""
    order = {m["participant"]: i for i, m in enumerate(mains)}
    spoken = listing(room.get("messages"))
    found = {}
    for index, pair in enumerate(map(mapping, listing(room.get("pairs")))):
        sender, recipient = pair.get("sender"), pair.get("recipient")
        if sender in order and recipient in order and sender != recipient:
            left, right = sorted((sender, recipient), key=order.get)
            link = found.setdefault((left, right), {"left": left, "right": right, "forward": 0, "back": 0,
                                                    "new": 0, "seen": index})
            link["forward" if sender == left else "back"] += valid_count(pair.get("sent")) or 0
            link["new"] += valid_count(pair.get("pending")) or 0
            link["seen"] = index
    for link in found.values():
        link["last"] = max((i for i, x in enumerate(spoken) if pair_message(x, link["left"], link["right"])), default=-1)
    return sorted(found.values(), key=lambda l: (not l["new"], -l["last"], -l["seen"]))


def pair_message(message, first, second):
    message = mapping(message)
    targets = listing(message.get("targets"))
    return (message.get("sender") == first and second in targets) or (message.get("sender") == second and first in targets)


def link_counts(link, unicode):
    right, left = ("→", "←") if unicode else ("->", "<-")
    parts = [mark + str(link[key]) for mark, key in ((right, "forward"), (left, "back")) if link[key]]
    return " ".join(parts) + (" · %s new" % link["new"] if link["new"] else "")


def short_name(label, room):
    """The board's name for a main, shortened to `room` cells while its "#N" or "#id" stays visible."""
    if display_width(label) <= room:
        return label
    first, _, rest = label.partition(" ")
    if first.startswith("#") and rest:
        keep = room - display_width(first) - 1
        return first + " " + fit(rest, keep) if keep >= 3 else first if display_width(first) <= room else ""
    rest, _, last = label.rpartition(" ")
    if last.startswith("#") and rest:
        keep = room - display_width(last) - 1
        return fit(rest, keep) + " " + last if keep >= 3 else last if display_width(last) <= room else ""
    return fit(label, room) if room > 0 else ""


def link_row(link, centres, labels, width, unicode):
    """The pair drawn between its lane columns: the line crosses mains in between and the counts sit on it.

    None when an end has no lane or both ends share a column, so the caller writes the pair as text."""
    start, end = centres.get(link["left"]), centres.get(link["right"])
    if start is None or end is None or end - start < 12:
        return None
    cells = [" "] * width
    cells[0] = cells[-1] = "│" if unicode else "|"
    bar, cross = ("─", "┼") if unicode else ("-", "+")
    cells[start:end + 1] = [bar] * (end - start + 1)
    cells[start] = ("◀" if unicode else "<") if link["back"] else bar
    cells[end] = ("▶" if unicode else ">") if link["forward"] else bar
    stops = {start, end}
    for column in set(centres.values()):
        if start < column < end:
            cells[column] = cross
            stops.add(column)
    stops = sorted(stops)
    text = link_counts(link, unicode)
    gap = max(((a + 1, b - 1) for a, b in zip(stops, stops[1:])), key=lambda g: g[1] - g[0])
    if gap[1] - gap[0] + 1 < len(text) + 2:
        return None
    first = gap[0] + (gap[1] - gap[0] + 1 - len(text)) // 2
    cells[first - 1:first + len(text) + 1] = list(" " + text + " ")
    for ident, edge, step in ((link["left"], start - 1, -1), (link["right"], end + 1, 1)):
        tag = short_name(labels[ident], edge - 1 if step < 0 else width - 3 - edge)
        place = edge - len(tag) if step < 0 else edge + 1
        if tag and 0 < place and place + len(tag) < width - 1:
            cells[place:place + len(tag)] = list(tag)
    return "".join(cells)


def pair_detail(member, room, width, labels):
    """The last six messages between two mains, oldest first, from the snapshot's mail between mains."""
    link = member["_link"]
    left, right = link["left"], link["right"]
    lines = wrapped("%s · %s" % (member["model"], link_counts(link, True)), width, 2)
    found = [m for m in listing(room.get("main_messages")) or listing(room.get("messages"))
             if pair_message(m, left, right)][-6:]
    for m in found:
        sender = m.get("sender")
        line = "%s %s → %s: %s" % (clock_stamp(m.get("ts")), labels[sender], labels[right if sender == left else left],
                                   readable(m.get("text"), " ") or "(empty)")
        lines += ["  " * (n > 0) + part for n, part in enumerate(wrapped(line.strip(), width, 3))]
    if len(found) < min(6, link["forward"] + link["back"]):
        lines.append("Older messages are not in this snapshot")
    return lines or ["No messages recorded"]


def result_handoff(room, message):
    """A result-<call> message counts only as that call's own handoff."""
    message = mapping(message)
    sender = str(message.get("sender"))
    # The room refuses any other result-<call> message at write time; messages kept from before that rule are
    # read the same way, so a handoff addressed to some other main than the call's parent is ignored.
    # Fail closed: a sender, parent or recipient that cannot be proven never marks a failure handled.
    parent = next((mapping(p).get("parent") for p in listing(room.get("participants"))
                   if mapping(p).get("participant") == sender and mapping(p).get("role") != "main"), None)
    return (str(message.get("id", "")) == "result-" + sender and message.get("message_kind") == "handoff"
            and bool(parent) and message.get("recipient") == parent)


def handled_results(room):
    """Ids of result-<call> handoffs its main has read, from the messages or the full-projection call_results."""
    # Only the call's own result handoff, read by its main, counts as a handled failure.
    read = {mapping(m).get("id") for m in listing(room.get("messages")) if result_handoff(room, m)
            and not listing(mapping(m).get("pending_for"))}
    return read | {"result-" + str(who) for who, r in mapping(room.get("call_results")).items()
                   if mapping(r).get("read") is True}


def failure_handled(report, call, read_results=None):
    """Only consumed result handoffs or an owner's accepted/completed task decision settle failures."""
    read_results = handled_results(mapping(report.get("room"))) if read_results is None else read_results
    return (call["state"] in ATTENTION_STATES - {"waiting_input", "waiting_approval", "review", "blocked"}
            and ("result-" + str(call.get("participant")) in read_results
                 or mapping(report.get("finalized")).get(call.get("task_id")) in {"completed", "accepted"}))


def settler(report):
    """(settled, aged) for calls: finished, aged-out or handled calls leave the board; shared with the tree."""
    room = mapping(report.get("room"))
    read_results = handled_results(room)
    finalized = mapping(report.get("finalized"))
    attempts_seen = mapping(report.get("attempts"))
    # Only complete active evidence permits hiding an old unknown; it does not prove completion.
    complete = attempts_seen.get("available") is True and valid_count(attempts_seen.get("active")) is not None and (
        valid_count(attempts_seen.get("active")) <= len(listing(attempts_seen.get("active_recent"))))

    def aged(call):
        # The existing ten-minute display cutoff never changes the unknown lifecycle state.
        return complete and (joined_seconds(call) or 0) > 600

    def settled(call):
        # Finished or aged-out calls, and failed ones whose result its main has read or finalized, leave the board.
        # A review or a block ends only with the owner's recorded decision on the task, never by reading a result.
        closed = finalized.get(call.get("task_id")) in {"completed", "accepted"}
        return (call["state"] in FINISHED or call["state"] == "presence unknown" and aged(call)
                or failure_handled(report, call, read_results) or call["state"] in {"review", "blocked"} and closed)
    return settled, aged


def call_classifier(report):
    """Display buckets only; admission and lifecycle evidence retain their own authority."""
    settled, _ = settler(report)
    awaiting = set(listing(report.get("awaiting_admission")))

    def classify(call):
        if call["state"] == "review" and not settled(call) or (call["state"] == "done" and call.get("participant") in awaiting):
            return "review"
        return "past" if settled(call) else "live"
    return classify


CALL_GROUPS = (("review", "Needs review"), ("past", "Past work"))


def call_order(call):
    """Finished calls first in completion order, then calls needing attention, running calls last."""
    state = call["state"]
    group = 0 if state in FINISHED else 2 if state in LIVE_STATES else 1
    stamp = (call.get("ended") if group == 0 else None) or call.get("joined_at") or ""
    return (group, str(stamp), str(call.get("joined_at") or ""), call.get("seq", 0), str(call.get("participant")))


def clock_stamp(ts, today=None):
    """Local "HH:MM", or "MM-DD HH:MM" when the day is not today."""
    try:
        local = time.localtime(calendar.timegm(time.strptime(str(ts), "%Y-%m-%dT%H:%M:%SZ")))
    except ValueError:
        return ""
    same = time.strftime("%m-%d", local) == time.strftime("%m-%d", today or time.localtime())
    return time.strftime("%H:%M" if same else "%m-%d %H:%M", local)


def call_span(call, arrow="->"):
    """"14:02 -> 14:31 finished" / "14:05 -> running"; the times come from the room, the end state from the call."""
    state = call["state"]
    words = said(state)
    if state == "failed" and call.get("guard"):
        words = "guard stop"
    elif state == "failed" and call.get("exit") is not None:
        words = ("timed out" + (" · continue with --continue" if call.get("role") == "worker" else "")) if call["exit"] == 124 \
            else "failed (exit %s)" % call["exit"]
    if state in LIVE_STATES or not call.get("ended"):
        end = ""
    else:
        end = clock_stamp(call["ended"])
    start = clock_stamp(call.get("joined_at"))
    if state == "presence unknown":
        return (start + " " + arrow + " " if start else "") + words
    if not start:
        return (words + " " + end).strip()
    return "%s %s %s%s" % (start, arrow, end + " " if end else "", words)


def joined_seconds(call):
    try:
        return time.time() - calendar.timegm(time.strptime(str(call.get("joined_at")), "%Y-%m-%dT%H:%M:%SZ"))
    except ValueError:
        return None


def live_unread(report, members, settled):
    """Unread mail addressed to mains and to their calls still working or waiting; the board and the tree both state it.

    Calls stay joined after they end, so a broadcast to "all" stays pending for them forever; only a call that can
    still read its mail counts."""
    mains = [m for m in members if m.get("role") == "main"]
    owners = {key for m in mains for key in (m["participant"], m.get("attempt")) if key}
    # A call in review or blocked has ended its process; broadcasts that keep arriving for it are never read.
    reading = LIVE_STATES | {"waiting_input", "waiting_approval"}
    ids = {m["participant"] for m in mains} | {m["participant"] for m in members if m.get("role") != "main"
                                              and m.get("parent") in owners and m.get("state") in reading
                                              and not settled(m)}
    return sum(valid_count(mapping(p).get("pending")) or 0
               for p in listing(mapping(report.get("room")).get("pairs")) if mapping(p).get("recipient") in ids)


def plan_idle(report):
    return valid_count(mapping(report.get("plan")).get("idle_days"))


def plan_summary(report):
    """(goal, verified, total, review, claimed) of the active plan; None when there is no goal or it idled a week."""
    plan = mapping(report.get("plan"))
    goal = clean(plan.get("goal"), 200)
    if not plan.get("present") or not goal or (plan_idle(report) or 0) >= 7:
        return None
    by_state = mapping(plan.get("by_state"))
    number = lambda *names: sum(valid_count(by_state.get(name)) or 0 for name in names)
    total = valid_count(plan.get("task_count"))
    return (goal, number("done"), total if total is not None else sum(valid_count(v) or 0 for v in by_state.values()),
            number("review", "landing"), number("claimed", "running"))


def goal_banner(report, width, unicode):
    """The goal rows at the top of every board as [(text, style)]; the progress always stays on the first row."""
    rows = goal_rows(report, width, unicode)
    land = mapping(report.get("land"))
    if land.get("active") is True:
        sep = " · " if unicode else " / "
        minutes = valid_count(land.get("minutes"))
        text = "LANDING " + clean(str(land.get("sha") or ""))[:7] + sep + clean(str(land.get("step") or "land"))[:12]
        rows.append((clipped(text + (sep + "%sm" % minutes if minutes is not None else ""), width), "head"))
    return rows


def goal_rows(report, width, unicode):
    summary = plan_summary(report)
    sep = " · " if unicode else " / "
    if summary is None:
        return [(clipped(("◎ " if unicode else "@ ") + "No shared goal" + sep + "oms agent-plan init --goal TEXT", width), "dim")]
    goal, done, total, review, claimed = summary
    filled = min(10, 10 * done // total) if total else 0
    bar = ("▕" + "█" * filled + "░" * (10 - filled) + "▏") if unicode else "[" + "#" * filled + "." * (10 - filled) + "]"
    prefix = ("◎ " if unicode else "@ ") + "GOAL  "
    counts = "%s/%s verified" % (done, total)
    # A claim is not proof of running work, and work waiting for review is progress: review outlives the bar.
    reviewing = sep + "%s review" % review if review else ""
    options = [bar + " " + counts + reviewing + sep + "%s claimed" % claimed, bar + " " + counts + reviewing,
               counts + reviewing, "%s/%s%s" % (done, total, reviewing), "%s/%s" % (done, total)]
    right = next((o for o in options if width - display_width(o) - 2 - display_width(prefix) >= 12), options[-1])
    room = max(1, width - display_width(right) - 2)
    if display_width(prefix + goal) <= room:
        text = prefix + goal
        return [(text + " " * max(2, width - display_width(text) - display_width(right)) + right, "head")]
    first, used = "", 0
    words = goal.split(" ")
    for index, word in enumerate(words):
        gap = 1 if first else 0
        if used + gap + display_width(word) > room - display_width(prefix):
            break
        first += " " * gap + word
        used += gap + display_width(word)
    else:
        index = len(words)
    rest = " ".join(words[index:])
    if not first:
        # One unbroken word wider than the row: cut it by cells and carry the remainder down.
        cut = fit(rest, room - display_width(prefix)).rstrip(ELLIPSIS[0])
        first, rest = cut, rest[len(cut):]
    head = prefix + first
    rows = [(head + " " * max(2, width - display_width(head) - display_width(right)) + right, "head")]
    if rest:
        rows.append((clipped(" " * display_width(prefix) + rest, width), "head"))
    return rows


def claims_by_main(report):
    """Plan tasks a main holds, by its room participant; the plan is repository-owned data."""
    held = {}
    for task in listing(mapping(report.get("plan")).get("tasks")):
        task = mapping(task)
        if task.get("claimed_by") and task.get("state") not in ("done", "ready"):
            held.setdefault(task["claimed_by"], []).append(task)
    return held


def ago(ts):
    try:
        seconds = max(0, time.time() - calendar.timegm(time.strptime(str(ts), "%Y-%m-%dT%H:%M:%SZ")))
    except ValueError:
        return ""
    return "just now" if seconds < 60 else "%dm ago" % (seconds // 60) if seconds < 3600 else \
        "%dh ago" % (seconds // 3600) if seconds < 86400 else "%dd ago" % (seconds // 86400)


def native_advisors(report, members):
    """Claude Code's built-in advisor as a card, running or just answered; it never enters the room."""
    calls = mapping(report.get("native_advisors"))
    return [{"participant": "native-advisor-" + m["participant"], "role": "advisor", "parent": m["participant"],
             "provider": "claude", "joined": True, "seq": 0, "title": "Built-in advisor",
             "state": "working" if mapping(calls.get(m["participant"])).get("running", True) else "done",
             "model": clean(mapping(calls.get(m["participant"])).get("model")) or "Advisor",
             "_native": clean(mapping(calls.get(m["participant"])).get("started")) or "earlier",
             "_finished": clean(mapping(calls.get(m["participant"])).get("finished")) or None}
            for m in members if m.get("role") == "main" and m.get("provider") == "claude"
            and m.get("state") not in FINISHED and isinstance(calls.get(m["participant"]), dict)]


def card(title, content, width, height, unicode):
    lines = [box_edge(title, width, unicode)]
    for line in content[:height - 2]:
        lines.append(box_row(line, width, unicode))
    lines += [box_row("", width, unicode)] * max(0, height - len(lines) - 1)
    return lines + [box_edge("", width, unicode, "bottom")]


def description(member, frame, unicode, width):
    model = member.get("model")
    name = MODEL_NAMES.get(model, model) or member.get("provider", "unknown")
    title = wrapped(member.get("title") or member["participant"], width - 4, 2)
    state = "exited" if member.get("role") == "main" and member["state"] == "done" else member["state"]
    return title + ["%s %s / %s" % (activity(state, frame, unicode), name, state),
                    "@ " + location_label(member.get("location")),
                    "id: " + member["participant"]]


JOINTS = {frozenset("ud"): "│", frozenset("lr"): "─", frozenset("udlr"): "┼", frozenset("dlr"): "┬",
          frozenset("ulr"): "┴", frozenset("udr"): "├", frozenset("udl"): "┤", frozenset("dr"): "┌",
          frozenset("dl"): "┐", frozenset("ur"): "└", frozenset("ul"): "┘"}


def connections(width, root, points, live, frame, unicode, upward=False, rows=1):
    """Only admitted parent IDs draw branches; a particle moves along one live edge toward its consumer."""
    if not points:
        return []
    child, parent = ("u", "d") if upward else ("d", "u")
    low, high = min(points + [root]), max(points + [root])
    branch = [" "] * width
    for col in range(low, min(high, width - 1) + 1):
        sides = ({"l"} if col > low else set()) | ({"r"} if col < high else set())
        sides |= ({child} if col in points else set()) | ({parent} if col == root else set())
        branch[col] = (JOINTS.get(frozenset(sides), "─") if unicode else
                       "|" if sides == {"u", "d"} else "-" if sides <= {"l", "r"} else "+")
    if live and frame is not None:
        target = live[(frame // max(1, width // 3)) % len(live)]
        path = list(range(min(root, target), max(root, target) + 1))
        if (target > root) if upward else (target < root):
            path.reverse()
        branch[min(width - 1, path[frame % len(path)])] = "●" if unicode else "o"
    line = "".join(branch).rstrip()
    stem = " " * root + ("│" if unicode else "|")
    return [line] if rows < 2 else [line, stem] if upward else [stem, line]


def joint(line, column, glyph):
    """Attach a wire at the first free edge cell from column; titles are never displaced."""
    used = 0
    for index, char in enumerate(line):
        if used >= column and char in "─-" and index < len(line) - 1:
            return line[:index] + glyph + line[index + 1:], used
        used += display_width(char)
    return line, column


def tail(text, width):
    """The last `width` columns of text, led by the active ellipsis when anything was cut."""
    if display_width(text) <= width:
        return text
    from dashboard_projection import ELLIPSIS
    mark, kept = ELLIPSIS[0], ""
    for ch in reversed(text):
        if display_width(ch + kept) > width - len(mark):
            break
        kept = ch + kept
    return mark + kept


def padded(text, width):
    text = clipped(text, width)
    return text + " " * max(0, width - display_width(text))


def layout(width, count, centered=False):
    span = min(58, width - 8) if centered else min(58, (width - count - 1) // count)
    return span, (width - (span * count + count - 1)) // 2


WORDS = {"working": "running", "live marker": "running", "verifying": "checking", "done": "finished",
         "timed_out": "timed out", "orphaned": "lost", "waiting_input": "needs input",
         "waiting_approval": "needs approval", "review": "in review", "presence unknown": "status unknown"}
WEEKDAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
ROLES = {"main": "Main", "advisor": "Advisor", "reviewer": "Reviewer", "council": "Debate", "worker": "Worker"}
MACHINE = re.compile(r"^(stop-reason:|model-route:|model-result:|usage detail:|tokens used|served model|cost usd|"
                     r"##\s*(output|exit|verify)\b|---\s*(begin|end) )", re.I)


def who(member, labels=None):
    """A participant named with its role, so a Sol worker never reads like the Sol main."""
    if not member:
        return "someone"
    name = mapping(labels).get(member.get("participant")) or model_name(member)
    return name + " " + ROLES.get(member.get("role"), "Worker").lower()


def said(state):
    return WORDS.get(state, str(state or "unknown").replace("_", " "))


def readable(text, joiner="\n", paragraphs=False):
    """Text for people: machine status lines and markdown marks are dropped, call exits become words."""
    output = []
    for line in str(text or "").splitlines():
        line = line.strip()
        exited = re.match(r"Call exit=(\d+); parent acceptance pending\.?\s*(.*)", line)
        if exited:
            line = (("Finished. " if exited.group(1) == "0" else "Failed (exit %s). " % exited.group(1)) + exited.group(2)).strip()
        if not line:
            if paragraphs and output and output[-1]:
                output.append("")
            continue
        if MACHINE.match(line):
            continue
        line = re.sub(r"\*\*|__|`", "", re.sub(r"^#+\s*", "", line))
        output.append(line)
    return joiner.join(output).strip()


def without_now(text):
    return re.sub(r"^(?:now\s*[:·-]\s*)+", "", text, flags=re.I)


def declared_status(room, participant, width):
    """A main's own declared task as "Now: text (HH:MM)" lines; the text is peer data."""
    found = (room.get("statuses") or {}).get(participant) or {}
    text = without_now(readable(found.get("text"), " "))
    try:
        stamp = time.strftime("%H:%M", time.localtime(calendar.timegm(time.strptime(str(found.get("ts")), "%Y-%m-%dT%H:%M:%SZ"))))
    except ValueError:
        stamp = ""
    return wrapped("Now: " + text + (" (" + stamp + ")" if stamp else ""), width, 2) if text else []


def detail(member, preview, width, members=(), room=None, teams=None, everyone=None, labels=None):
    """Selected block content in plain words; reading mail here never acknowledges it."""
    room = room or {}
    if member.get("role") == "pair":
        return pair_detail(member, room, width, labels), {}
    names = {m["participant"]: who(m, labels) for m in members}
    name = mapping(labels).get(member.get("participant")) or model_name(member)
    state = "exited" if member.get("role") == "main" and member.get("state") == "done" else member.get("state")
    title = clean(member.get("title")) or ""
    title = "" if title in ("Native task not recorded", "Task unrecorded") else title
    where = "in the repository" if member.get("location") in (None, "repository") else "in its own worktree"
    targets = {}
    if member.get("role") == "main":
        lines = wrapped("%s · %s main · %s" % (name, (member.get("provider") or "").capitalize(), said(state)), width, 2)
        lines += wrapped("Task: " + (title or "no task title yet"), width, 2)
        lines += declared_status(room, member["participant"], width)
        if member.get("owns"):
            lines += wrapped("Scope: " + ", ".join(clean(scope) for scope in listing(member["owns"])), width, 2)
        if not member.get("model") and member.get("model_at_launch"):
            lines += wrapped("Current model unavailable · launch setting: " +
                             MODEL_NAMES.get(member["model_at_launch"], member["model_at_launch"]), width, 2)
        inbox = [m for m in listing(room.get("messages")) if member["participant"] in listing(m.get("targets"))]
        unread = sum(member["participant"] in listing(m.get("pending_for")) for m in inbox)
        calls = sorted((everyone or teams or {}).get(member["participant"], []), key=call_order)
        attention = [c for c in calls if c["state"] in ATTENTION_STATES]
        active = [c for c in calls if c["state"] in LIVE_STATES]
        finished = [c for c in calls if c["state"] in FINISHED]
        unknown = [c for c in calls if c["state"] not in ATTENTION_STATES | LIVE_STATES | FINISHED]
        counts = [(len(group), label) for group, label in ((active, "active"), (attention, "need attention"),
                  (finished, "finished"), (unknown, "no record")) if group]
        lines += ["", "Team: " + (" · ".join("%s %s" % pair for pair in counts) or "no calls recorded")]

        def team_rows(heading, group):
            if group:
                lines.append("  " + heading)
            for c in group:
                label = MODEL_NAMES.get(c.get("model"), c.get("model")) or "unknown"
                # Every wrapped team line keeps its exact result target, including finished calls.
                for part in wrapped("- %s%s %s · %s · %s" % ("! " if c["state"] in ATTENTION_STATES else "", ROLES.get(c.get("role"), "Worker"),
                                                           label, call_span(c, "→") if c.get("joined_at") or c.get("ended") else said(c["state"]),
                                                           clean(c.get("title")) or "no task title"), width - 2, 2):
                    targets[len(lines)] = action(c)
                    lines.append("  " + part)
        if member.get("started_by"):
            starter = next((m for m in members if m["participant"] == member["started_by"]), {})
            lines.append("Started by %s·%s" % (model_name(starter) if starter else "another main", member["started_by"][-4:]))
        lines += ["", "Messages to %s: %s%s" % (who(member, labels), len(inbox), " (%s unread)" % unread if unread else "")]
        for m in inbox[-3:]:
            lines += wrapped("  From %s: %s" % (names.get(m.get("sender"), clean(m.get("sender"))),
                                                readable(m.get("text"), " ") or "(empty)"), width, 1)
        if not inbox:
            lines.append("  No messages received")
        lines.append("")
        # The team reads like a log: finished calls on top, then what needs attention, live work last.
        team_rows("Finished", finished)
        team_rows("Needs attention", attention)
        team_rows("No record", unknown)
        team_rows("Active", active)
        lines += ["", "Works %s%s" % (where, " · effort " + member["effort"] if member.get("effort") else "")]
        native = next((c for c in calls if c.get("_native")), None)
        if native:
            lines.append("Built-in advisor: " + ("asked %s · running" % native["_native"] if native["state"] != "done"
                                                 else "last answered %s" % (native.get("_finished") or native["_native"])))
        return lines + ["Click a team row to see it here · click the main again to open its chat"], targets
    if member.get("_native"):
        done = member["state"] == "done"
        return ["%s advisor · %s" % (member.get("model"), "answered" if done else "running"),
                ("Answered %s" % (member.get("_finished") or member["_native"]) if done else "Started %s" % member["_native"])
                + " · Claude Code's built-in advisor",
                "Its answer stays inside the main's chat"], targets
    owner = names.get(member.get("parent"), "")
    lines = wrapped("%s%s · %s%s" % (who(member), " for " + owner if owner else "",
                                     said(state), " · " + title if title else ""), width, 3)
    if member.get("round", 1) > 1:
        lines += wrapped("Round %s of this task, continuing %s" % (member["round"], names.get(member.get("continues"), "its earlier worker")), width, 2)
    if member.get("exit") == 124 and member.get("task_id"):
        lines += wrapped("Timed out. Continue it from its main: oms panel --dispatch worker --continue %s --access write --purpose implement --brief-file PATH --verify COMMAND"
                         % clean(member["task_id"]), width, 3)
    access = {"write": "can edit", "read": "read-only"}.get(member.get("access"), "permissions unrecorded")
    metadata = "%s %s%s" % (access.capitalize(), where, " · effort " + member["effort"] if member.get("effort") else "")
    loaded = preview.get("report")
    if preview.get("error"):
        return lines + ["", readable(preview["error"], " "), "", metadata], targets
    if loaded is None:
        return lines + ["", "Loading its recorded result...", "", metadata], targets
    if not listing(loaded.get("rows")):
        return lines + ["", "No result recorded yet (%s)" % said(state), "", metadata], targets
    row = listing(loaded.get("rows"))[0]
    answer = readable(latest_answer(loaded), paragraphs=True)
    outcome = {"accepted": "Accepted by its main", "completed": "Completed; the main recorded a summary",
               "failed": "Marked failed by its main"}.get(row.get("outcome"), "Not reviewed by its main yet")
    check = mapping(row.get("verification")).get("status")
    lines += ["", outcome + " · " + ("checks %s" % str(check).replace("_", " ") if check else "no checks run")]
    summary = readable(row.get("summary"), paragraphs=True)

    def result_text(heading, text, limit):
        body, more = [], False
        for paragraph in text.splitlines():
            if len(body) == limit:
                more = True
                break
            if not paragraph.strip():
                if body and body[-1]:
                    body.append("")
                continue
            cut = len(paragraph) > 4000
            for part in wrapped(paragraph, width - 2, limit + 1, max_chars=4000):
                if len(body) == limit:
                    more = True
                    break
                body.append("  " + part)
            more = more or cut
            if more:
                break
        lines.extend(["", heading] + body)
        if more:
            lines.append("  More in the full recorded result · f opens it")

    if summary:
        result_text("Main's summary:", summary, 24)
    if answer and answer != summary:
        result_text("Worker's answer:" if summary else "Answer:", answer, 12 if summary else 40)
    changes = next((c["changes"] for c in reversed(listing(row.get("calls"))) if c.get("changes")), None)
    if changes and changes.get("issue"):
        lines += ["", "Changed files: unavailable (" + changes["issue"] + ")"]
    elif changes and changes.get("count"):
        lines += ["", "Changed files: %s (+%s -%s)" % (changes["count"], changes["added"], changes["removed"])]
        lines += ["  %s +%s -%s" % tuple(f) for f in changes["files"][:12]]
        lines += ["  and %s more files" % (changes["count"] - 12)] if changes["count"] > 12 else []
    lines += ["", metadata]
    if str(member.get("location") or "").startswith("worktree:") and member.get("state") in FINISHED | {"failed"}:
        lines.append("w opens its worktree in a shell")
    return lines, targets


FINISHED = {"done", "cancelled"}
MAX_LANES = 6


def latest_answer(report):
    for row in listing(mapping(report).get("rows")):
        for call in reversed(listing(row.get("calls"))):
            if call.get("answer"):
                return call["answer"]
        if row.get("summary"):
            return row["summary"]
    return None


def render_graph(report, width, height, color=False, unicode=True, frame=None, menu=False,
                 attention_only=False, main_attempt=None, navigation=None, managed=False):
    """Page real parent relationships without inventing routers or progress."""
    from panel_view import tone
    from dashboard_projection import use_unicode
    use_unicode(unicode)
    interactive = navigation is not None
    navigation = navigation if interactive else {}
    navigation.pop("box", None)
    room = mapping(report.get("room"))
    members = nodes(report)
    members += native_advisors(report, members)
    labels = main_names(report, members)
    mains = window_order(report, [m for m in members if m.get("role") == "main"])
    selected = navigation.get("selected")
    owners = {key: m for m in mains for key in (m["participant"], m.get("attempt")) if key}
    primary = owners.get(main_attempt) or next((m for m in mains if m["state"] in LIVE_STATES), next(iter(mains), None))
    picked = next((m for m in members if selected and selected[1] == m["participant"]), None)
    if picked:
        primary = picked if picked.get("role") == "main" else owners.get(picked.get("parent"), primary)
    debates = [row for row in worker_rows(report) if row["source"] == "council"]

    def team(main, every=False):
        ids = {key for key in (main["participant"], main.get("attempt")) if key}
        calls = [{"participant": row["task"], "role": "council", "model": row["name"],
                  "title": row["work"], "state": row["state"], "parent": row["parent"],
                  "_action": ("debate", (row["task"], main["participant"]))}
                 for row in debates if row["parent"] in ids]
        calls += [m for m in members if m.get("role") != "main" and m.get("parent") in ids]
        calls = [m for m in calls if m["state"] in ATTENTION_STATES] if attention_only and not every else calls
        # Judges precede workers so keyboard order follows the drawing.
        return sorted(calls, key=lambda m: (m.get("role") == "worker",) + call_order(m))

    classify = call_classifier(report)
    teams = {m["participant"]: team(m, every=True) for m in mains}
    if attention_only:
        teams = {key: [c for c in calls if c["state"] in ATTENTION_STATES or classify(c) == "review"]
                 for key, calls in teams.items()}
    grouped = {key: {kind: [c for c in calls if classify(c) == kind] for kind, _ in CALL_GROUPS}
               for key, calls in teams.items()}
    if navigation.get("group_tree"):
        if any(grouped.get(owner, {}).get(kind) for kind, owner in navigation.get("expanded_groups", set())):
            from panel_tree import render_tree
            return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                               main_attempt, navigation, managed=managed)
        navigation.pop("group_tree", None)
    linked_children = team(primary, every=True) if primary else []
    # Finished calls leave the board (results stay in the main's detail and the tree); attention stays.
    settled, _ = settler(report)
    finished = {key: len(groups["past"]) for key, groups in grouped.items()}
    everyone = teams
    teams = {key: [c for c in calls if classify(c) == "live"] for key, calls in teams.items()}
    children = teams[primary["participant"]] if primary else []
    judges = [m for m in children if m.get("role") in {"advisor", "reviewer", "council"}]
    workers = [m for m in children if m.get("role") == "worker"]
    unlinked = [m for m in members if m.get("role") != "main" and m.get("parent") not in owners]
    offset = max(0, navigation.get("offset", 0))
    scrolled = mapping(navigation.get("band_offsets"))
    judge_start = min(scrolled.get("judge", offset), max(0, len(judges) - 3))
    worker_start = min(scrolled.get("worker", offset), max(0, len(workers) - 3))
    if selected != navigation.get("graph_selected"):
        for group, name in ((judges, "judge"), (workers, "worker")):
            index = next((i for i, m in enumerate(group) if action(m) == selected), None)
            if index is not None:
                if name == "judge":
                    judge_start = index // 3 * 3
                else:
                    worker_start = index // 3 * 3
    shown_judges, shown_workers = judges[judge_start:judge_start + 3], workers[worker_start:worker_start + 3]
    pins = navigation.get("pinned")
    if pins and not any(m["participant"] in pins for m in mains):
        pins = navigation["pinned"] = None  # pins saved for another room do not apply here
    cap = width // 30

    def grid_columns(count):
        # Four to six mains fill two rows when each lane keeps 30 columns and the board is tall enough.
        cols = (count + 1) // 2
        return cols if 4 <= count <= MAX_LANES and height >= 30 and (width - cols + 1) // cols >= 30 else 0

    if (pins is None and (not main_attempt or navigation.get("expanded") or navigation.get("overview"))
            and (2 <= len(mains) <= cap or grid_columns(len(mains)))):
        # The live board, the control window or an expanded board overviews every main that fits.
        pins = [m["participant"] for m in mains]
    navigation["auto_pins"] = pins if navigation.get("pinned") is None else None
    pinned = [m for m in mains if m["participant"] in (pins or ())]

    def lane_set(limit):
        chosen = pinned[:limit] if len(pinned) >= 2 and height >= 20 else []
        if chosen and primary in mains and primary not in chosen:
            # The selected main always has a lane, so the tab marker never points at an undrawn main.
            chosen = [m for m in mains if m in chosen[:limit - 1] or m is primary]
        return chosen

    grid_cols = grid_columns(min(len(pinned), MAX_LANES))
    lanes = lane_set(MAX_LANES if grid_cols else cap)
    if grid_cols and len(lanes) != len(pinned):
        grid_cols = grid_columns(len(lanes))
    grid = bool(grid_cols)
    preview = mapping(navigation.get("preview"))
    links = main_links(room, mains)
    joiner = " ⇄ " if unicode else " <> "
    pair_members = [{"participant": "pair:%s:%s" % (l["left"], l["right"]), "role": "pair", "_link": l,
                     "_action": ("pair", (l["left"], l["right"])),
                     "model": labels[l["left"]] + joiner + labels[l["right"]]} for l in links[:4] if height >= 32]
    previewed = next((m for m in mains + [c for calls in everyone.values() for c in calls] + pair_members
                      if action(m) == preview.get("target")), None)
    navigation.pop("full_result", None)
    if previewed and previewed.get("role") not in ("main", "pair") and not previewed.get("_native"):
        navigation["full_result"] = action(previewed)
    if menu:
        footer = ["[1] Codex  [2] Claude  [h] Chats  [?] More  [q] " + ("Detach" if managed else "Quit")]
    else:
        chosen_member = next((m for m in mains + [c for calls in everyone.values() for c in calls]
                              if action(m) == selected), None)
        footer = footer_hints(navigation, width, managed, unicode, chosen_member, len(mains), True)
    hints = list(footer)
    hint_rows = len(footer)
    if navigation.get("notice"):
        if height >= 20 or menu:
            footer.append(clean(navigation["notice"], 180))
        else:
            # A short board shows the notice in place of the hints, never silently.
            fixed = "  ".join(footer_hints({}, width, managed, unicode, None, 0, True)[-1].split("  ")[-2:])
            footer = [clipped(clean(navigation["notice"], 180), max(1, width - display_width(fixed) - 2)) + "  " + fixed]
            hint_rows = 1
    publications = mapping(room.get("publications"))
    if publications:
        footer.append("APP DELIVERY / " + " | ".join("%s: %s%s" % (
            target, mapping(receipt).get("status", "unknown"),
            " (unconfirmed)" if mapping(receipt).get("delivery_unknown") else "")
            for target, receipt in list(publications.items())[-3:]))
    bar = None if menu else spawn_bar(width, navigation)
    if bar:
        footer.append(bar[0])
    if not menu:
        # The key hints are always the board's last rows; notices, delivery and the start bar sit just above.
        footer = footer[hint_rows:] + footer[:hint_rows]
    lines, hits, bands_hit = [], [], []

    def add(value, style=None):
        value = clipped(value, width)
        lines.append(PALETTE[style] + value + "\033[0m" if color and style else value)

    def paint(text, style):
        return PALETTE[style] + text + "\033[0m" if color and style else text

    def chosen(text, style):
        # A selection is a block in its own colour, never a plain white inversion.
        return PALETTE[style] + "\033[7m" + text + "\033[0m" if color else text

    def hue(m):
        return "codex" if m.get("provider") == "codex" else "main"

    def name_of(m):
        return labels.get(m["participant"]) or model_name(m)

    def state_of(m):
        return "exited" if m.get("role") == "main" and m["state"] == "done" else m["state"]

    claims = claims_by_main(report)

    def claim_line(m):
        held = claims.get(m["participant"], [])
        if not held:
            return ""
        return "Task: %s · %s" % (held[0].get("id"), held[0].get("state")) + (" +%s" % (len(held) - 1) if len(held) > 1 else "")

    def declared_of(m):
        # The card already labels it "Now:"; a status written as "Now: ..." would read twice.
        return without_now(readable(mapping(mapping(room.get("statuses")).get(m["participant"])).get("text"), " "))

    def now_line(m):
        """"Now: text" with the status age once it is older than 30 minutes; the text is peer data."""
        text = declared_of(m)
        stamp = mapping(mapping(room.get("statuses")).get(m["participant"])).get("ts")
        try:
            seconds = time.time() - calendar.timegm(time.strptime(str(stamp), "%Y-%m-%dT%H:%M:%SZ"))
        except ValueError:
            seconds = 0
        old = seconds > 1800
        # Past an hour the main's own words no longer describe the live counts beside them.
        return ("Now: " + text + ((" · " if unicode else " / ") + ago(stamp) + (" (stale)" if seconds > 3600 else "") if old else ""),
                old) if text else ("", False)

    def task_of(m):
        task = clean(m.get("title")) or "Task unrecorded"
        if task not in ("Native task not recorded", "Task unrecorded"):
            return task
        if m.get("role") == "main":
            return " · ".join(part for part in (claim_line(m), now_line(m)[0]) if part) \
                or "no status yet"
        return "no task title yet"

    def task_lines(m, span, limit):
        title = task_of(m)
        now = declared_status(room, m["participant"], span) if m.get("role") == "main" else []
        if now and clean(m.get("title")) not in (None, "", "Native task not recorded", "Task unrecorded"):
            if limit == 1:
                return [clipped(title, max(1, span // 2 - 2)) + " / " + clipped(now[0], max(1, span // 2 - 1))]
            return wrapped(title, span, 1) + now[:limit - 1]
        return wrapped(title, span, limit)

    # Mail to finished calls is never read again; unread mail to mains and running calls is counted.
    unread_total = live_unread(report, members, settled)
    heading_text = "OMS · " + (clean(room.get("title") or room.get("id")) or "Work room") + " · %s main%s · %s" % (
        len(mains), "" if len(mains) == 1 else "s",
        "%s unread for live participants" % unread_total if unread_total else "no unread messages")
    clock = time.strftime("%m-%d ") + WEEKDAYS[time.localtime().tm_wday] + time.strftime(" %H:%M")
    if display_width(heading_text) + len(clock) + 2 <= width:
        heading_text += " " * (width - display_width(heading_text) - len(clock)) + clock
    if height >= 10:
        for text, style in goal_banner(report, width, unicode):
            hits.append({"y": len(lines) + 1, "x1": 1, "x2": width, "action": ("tab", "plan")})
            add(text, style)
    add(heading_text, "head")
    for alert in status_alerts(report):
        add(alert, "bad")
    if height >= 26:
        from panel_metrics import percent
        readings = mapping(report.get("provider_status"))
        known = []
        for key, title in (("claude", "Claude"), ("codex", "Codex")):
            row = mapping(readings.get(key))
            week, context = percent(row.get("weekly_used")), percent(row.get("context_left"))
            if week is not None or context is not None:
                known.append("%s %s week · ctx %s" % (
                    title, "?" if week is None else "%s%%" % week, "?" if context is None else "%s%%" % context))
        add(" │ ".join(known) if known else "Usage readings unavailable (Claude, Codex)", "dim")
    if height >= 20 and mains:
        # Tabs select a main locally; only the already-shown main's tab opens its chat.
        y, x, value = len(lines) + 1, 0, ""
        tab_ids = {m["participant"] for m in mains}
        multiple = len(mains) > 1
        if multiple:
            every = len(pinned) == len(mains)
            text = ("[x All] " if every else "[+ All] ")
            hits.append({"y": y, "x1": 1, "x2": len(text) - 1, "action": ("pin", "*")})
            value, x = paint(text, "dim"), len(text)
        room_width = width - x
        pieces = []
        for number, m in enumerate(mains, 1):
            calls = teams[m["participant"]]
            title = clean(m.get("title"))
            task = "" if not title or title == "Native task not recorded" else title
            alerts = sum(c["state"] in ATTENTION_STATES for c in calls) + len(grouped[m["participant"]]["review"])
            # Unread mail addressed to this main, including messages from other mains.
            unread = sum(valid_count(mapping(p).get("pending")) or 0 for p in listing(room.get("pairs"))
                         if mapping(p).get("recipient") == m["participant"])
            asked = open_count(room, m["participant"])
            badge = " ".join("%s%s" % (mark, n) for mark, n in (
                ("W", sum(c.get("role") == "worker" for c in calls)),
                ("A", sum(c.get("role") != "worker" for c in calls)), ("!", alerts), ("M", unread),
                ("?", asked)) if n)
            ctx = context_note(report, m)
            if ctx:
                badge += (" " if badge else "") + "c%s%%" % ctx[0]
            name = name_of(m)
            pieces.append({
                "m": m, "task": task, "badge": badge, "alerts": alerts, "ctx": ctx, "name": name,
                "ident": name.split(" ", 1)[0] if re.match(r"#\d+ ", name) else "#%s" % number,
                "need": bool(alerts or unread or asked or state_of(m) in ("waiting_input", "waiting_approval")),
                "pin": ("[x]" if m in pinned else "[+]") if multiple else ""})

        def cell(p, step, cap):
            """The tab's body text for one shrink step; (a)/(b) clip the task to cap, (c) drops it, (d) one marker, (e) the number."""
            m = p["m"]
            mark = ("▸" if unicode else ">") if m == primary else " "
            lead = "%s%s %s" % (mark, activity(state_of(m), frame, unicode),
                                ("↳" if unicode else "^") if m.get("started_by") in tab_ids else "")
            if step == 4:
                return lead + p["ident"] + " "
            text = lead + p["name"]
            if step < 2 and p["task"]:
                text += ("·" if unicode else "/") + tail(p["task"], cap)
            if step < 3 and p["badge"]:
                text += " " + p["badge"]
            if step == 3 and p["need"]:
                text += " " + ("•" if unicode else "*")
            return text + " "

        def cells(step, cap=0):
            return [(cell(p, step, cap), "" if step == 4 else p["pin"]) for p in pieces]

        def total(row):
            return sum(display_width(body) + len(pin) + 1 for body, pin in row) - 1

        chosen_row = None
        longest = max(display_width(p["task"]) for p in pieces)
        for cap in range(max(4, min(36, longest)), 3, -1):
            if total(cells(1, cap)) <= room_width:
                chosen_row = cells(1, cap)
                break
        for step in (() if chosen_row else (2, 3, 4)):
            if total(cells(step)) <= room_width:
                chosen_row = cells(step)
                break
        if chosen_row is None:
            chosen_row = cells(4)
        lo, hi = 0, len(mains)
        if total(chosen_row) > room_width:
            at = mains.index(primary) if primary in mains else 0
            marks = len(str(len(mains)))

            def span(a, b):
                return total(chosen_row[a:b]) + (marks + 3 if a else 0) + (marks + 3 if b < len(mains) else 0)
            lo, hi = at, at + 1
            while True:
                grew = False
                for a, b in ((lo - 1, hi), (lo, hi + 1)):
                    if a >= 0 and b <= len(mains) and span(a, b) <= room_width:
                        lo, hi, grew = a, b, True
                        break
                if not grew:
                    break
        if lo:
            label = ("‹ %s" if unicode else "< %s") % lo
            hits.append({"y": y, "x1": x + 1, "x2": min(width, x + len(label)), "action": ("chat", mains[lo - 1]["participant"]),
                         "select": True})
            hold = any(pieces[i]["need"] for i in range(lo))
            value, x = value + paint(label, "alert" if hold else "dim") + " ", x + len(label) + 1
        for p, (body, pin) in zip(pieces[lo:hi], chosen_row[lo:hi]):
            m = p["m"]
            hits.append({"y": y, "x1": x + 1, "x2": x + display_width(body), "action": ("chat", m["participant"]),
                         "select": True})
            if pin:
                hits.append({"y": y, "x1": x + display_width(body) + 1, "x2": x + display_width(body) + 3,
                             "action": ("pin", m["participant"])})
            style = "alert" if p["alerts"] or (p["ctx"] and p["ctx"][1] == "alert") else hue(m)
            value += (chosen(body, style) if m == primary else paint(body, style)) + paint(pin, "dim") + " "
            x += display_width(body) + len(pin) + 1
        if hi < len(mains):
            label = ("%s ›" if unicode else "%s >") % (len(mains) - hi)
            hits.append({"y": y, "x1": x + 1, "x2": min(width, x + len(label)), "select": True,
                         "action": ("chat", mains[hi]["participant"])})
            hold = any(pieces[i]["need"] for i in range(hi, len(mains)))
            value += paint(label, "alert" if hold else "dim")
        lines.append(value if hi < len(mains) else value[:-1])
    group_actions = []
    if primary:
        for kind, title in CALL_GROUPS:
            calls = grouped[primary["participant"]][kind]
            if not calls:
                continue
            target = ("group", (kind, primary["participant"]))
            group_actions.append(target)
            unknown = sum(c["state"] == "presence unknown" for c in calls)
            label = "%s %s (%s) / %s" % ("▸" if unicode else ">", title, len(calls), name_of(primary))
            if unknown:
                label += " / %s earlier unknown" % unknown
            hits.append({"y": len(lines) + 1, "x1": 1, "x2": width, "action": target})
            add(label, "review" if kind == "review" else "dim")
    pair_at = len(lines)
    mains_by_id = {m["participant"] for m in mains}
    latest = next((m for m in map(mapping, reversed(listing(room.get("main_messages")) or listing(room.get("messages")))) if m.get("sender") in mains_by_id
                   and any(t in mains_by_id and t != m["sender"] for t in listing(m.get("targets")))), None)
    box_height = 2 + len(links[:4]) + (len(links) > 4) + bool(latest)
    if links and height >= 32:
        # Placeholder rows keep the budget honest; the box is drawn once the lanes are final.
        lines.extend([""] * box_height)
    elif links and height >= 20:
        pair_unread = sum(l["new"] for l in links)
        add("Between mains: %s pair%s · %s unread%s" % (
            len(links), "" if len(links) == 1 else "s", pair_unread,
            " of %s in room" % unread_total if unread_total > pair_unread else ""), "review")
    others = listing(room.get("other_rooms"))
    if room.get("other_rooms_incomplete") and height >= 20 and not others:
        add("OTHER ROOMS / scan incomplete; mains in other rooms may be missing", "alert")
    if others and height >= 20:
        # Rooms are bounded, so one panel can hold mains in several; never merge their graphs.
        add("OTHER ROOMS" + (" (scan incomplete)" if room.get("other_rooms_incomplete") else "") + " / " + " | ".join("%s %s live main%s%s" % (clean(r.get("id"), 24), r.get("mains"),
            "s" if r.get("mains") != 1 else "",
            " / %s pending" % r["pending"] if r.get("pending") else "") for r in others[:3]) +
            (" | +%s" % (len(others) - 3) if len(others) > 3 else "") + " / o Rooms in control", "alert")
    budget = max(0, height - int(menu) - len(footer))
    used = len(lines)
    band_count = 1 + bool(shown_judges) + bool(shown_workers)
    wire_rows = 2 if height >= 32 else 1
    wires = wire_rows * (bool(shown_judges) + bool(shown_workers))
    # Empty advisor/worker places stay visible, one row each, so the structure never changes shape.
    wires += (not shown_judges) + (not shown_workers) if primary and height >= 20 else 0

    def full_title(m):
        role = "MAIN" if m.get("role") == "main" else m.get("role", "worker").upper()
        return role + " / " + name_of(m)

    def heading(m):
        mark = ("▸ " if unicode else "> ") if action(m) == selected else ""
        return mark + full_title(m)

    def compact(group, span):
        # The top wire must meet the card midpoint, even with a selection mark.
        limit = span // 2 - 4 if any(m.get("role") in {"main", "worker"} for m in group) else span - 6
        return any(display_width(heading(m)) > limit for m in group)

    shown_groups = [g for g in (shown_judges, shown_workers) if g] + ([[primary]] if primary else [])

    def minimum_rows(group):
        span = layout(width, len(group), group == [primary])[0]
        return 4 + max(len(task_lines(m, span - 4, 2)) for m in group) if compact(group, span) else 3

    minimum_total = sum(minimum_rows(g) for g in shown_groups)
    embedded = {}
    if primary and not lanes and height <= 20 and budget - used - wires < minimum_total:
        main_span, main_left = layout(width, 1, True)
        middle = main_left + main_span // 2
        for name, group in (("judge", shown_judges), ("worker", shown_workers)):
            if not group:
                continue
            span, left = layout(width, len(group))
            points = [left + span // 2 + i * (span + 1) for i in range(len(group))]
            if not all(main_left < point < main_left + main_span - 1 for point in points):
                continue
            title_start = None
            if name == "judge":
                ports = sorted(set([main_left, middle, main_left + main_span - 1] + points))
                free = [(end - start - 1, start + 1) for start, end in zip(ports, ports[1:])]
                size, title_start = max(free)
                if size < display_width(heading(primary)) + 2:
                    continue
            embedded[name] = (points, group, title_start)
        # A short graph can reuse a free main border for a branch without losing card content.
        wires -= wire_rows * len(embedded)

    def fits(extra):
        return budget - used - extra >= (20 if grid else 6) if lanes else budget - used - wires - extra >= minimum_total

    def detail_rows():
        # The overview keeps one-line cards so the selected block's detail gets the rest.
        if not previewed:
            # The tab box stays after Esc, so Plan, Debate and Messages remain reachable.
            return 6 if interactive and fits(6) else 1 if interactive and fits(1) else 0
        room_left = budget - used - (max(20 if grid else 6, (budget - used) // 2) if lanes else wires + minimum_total) - 1
        return room_left if room_left >= 5 else 3 if fits(3) else 1 if interactive and fits(1) else 0

    reserve = detail_rows()
    if lanes and not fits(reserve):
        if grid:
            # Two rows do not fit: one row, as when the board is too narrow for a grid.
            grid = False
            lanes = lane_set(cap) if navigation.get("pinned") or len(mains) <= cap else []
            reserve = detail_rows()
        if lanes and not fits(reserve):
            lanes = []
            reserve = detail_rows()
    if not lanes and not fits(0):
        from panel_view import render
        navigation.update(hits=[], items=[], viewport=0, positions={})
        text = render(report, primary.get("provider", "codex") if primary else "codex", width, height,
                      color, unicode, menu=menu, managed=managed, main_attempt=main_attempt, frame=frame,
                      view="tree" if interactive else "compact", attention_only=attention_only, navigation=navigation)
        if publications and "APP DELIVERY" not in text:
            rows = text.splitlines()
            rows[-1] = clipped(footer[-1], width)
            text = "\n".join(rows)
        return text
    def extras(m):
        return 1 if m.get("role") == "main" or m.get("location") or m.get("effort") else 0

    def needed(group, centered=False):
        span = layout(width, len(group), centered)[0]
        reflow = int(compact(group, span))
        return max(3 + extras(m) + reflow + len(task_lines(m, span - 4, 2)) for m in group)

    # Cards never keep blank rows; rows they do not need go to the detail area.
    def drawn_rows(card_height):
        return sum(max(minimum_rows(g), min(card_height, needed(g, g == [primary]))) for g in shown_groups)

    available_cards = budget - used - wires - reserve
    card_height = next((candidate for candidate in range(6, 2, -1)
                        if drawn_rows(candidate) <= available_cards), 3)
    if reserve > 3 and (height < 9 * band_count + 20 or navigation.get("tab") in ("plan", "debate", "messages")):
        # Reading tabs get the rows: the cards shrink to one line.
        card_height, reserve = 3, budget - used - wires - (3 * band_count * (1 + grid) if lanes else minimum_total) - 1
    automatic = False
    if (interactive and not previewed and primary and not lanes and height >= 24
            and not navigation.get("dismissed")):
        # Rows the cards leave unused show the shown main by default; it never displaces a choice.
        drawn = drawn_rows(card_height)
        spare_rows = budget - used - wires - drawn - 1
        if spare_rows >= 5:
            previewed, preview, reserve, automatic = primary, {"target": action(primary), "report": {}}, spare_rows, True

    def cards(group, style, centered=False, top=False, bottom=False, before=None):
        count = len(group)
        cell_width, left = layout(width, count, centered)
        rows_h = max(minimum_rows(group), min(card_height, needed(group, centered)))
        reflow = compact(group, cell_width)
        blocks, points, cursor = [], [], left
        for m in group:
            span = cell_width
            title = heading(m)
            if reflow:
                role = {"advisor": "ADV", "reviewer": "REV", "worker": "WRK", "council": "DEB"}.get(m.get("role"), "MAIN")
                title = (("▸ " if unicode else "> ") if action(m) == selected else "") + role
            status = "%s %s" % (activity(state_of(m), frame, unicode), said(state_of(m)))
            if m.get("role") == "main" and open_count(room, m["participant"]):
                status += (" · " if unicode else " / ") + "? %s open" % open_count(room, m["participant"])
            if m.get("role") != "main" and primary:
                status += " · for " + clean(name_of(primary))
            task = task_of(m)
            content = [status + " / " + task] if rows_h == 3 else [status] + task_lines(m, span - 4, rows_h - 3)
            if rows_h >= 4:
                where = " / ".join(v for v in ("@ " + location_label(m["location"]) if m.get("location") else "",
                                                m.get("effort") or "") if v)
                if m.get("role") == "main":
                    calls = teams.get(m["participant"], [])
                    where = "Team W%s A%s%s" % (
                        sum(c.get("role") == "worker" for c in calls), sum(c.get("role") != "worker" for c in calls),
                        " / !%s attention" % sum(c["state"] in ATTENTION_STATES for c in calls)
                        if any(c["state"] in ATTENTION_STATES for c in calls) else "")
                task_rows = len(task_lines(m, span - 4, 2))
                tail = [where] if where and rows_h >= 4 + task_rows + int(reflow) else []
                content = ([full_title(m)] if reflow else []) + [status] + task_lines(
                    m, span - 4, max(1, min(2, rows_h - 3 - len(tail) - int(reflow)))) + tail
            block = card(title, content, span, rows_h, unicode)
            column = span // 2
            if top:
                block[0], column = joint(block[0], column, "┴" if unicode else "+")
            if bottom:
                block[-1] = joint(block[-1], span // 2, "┬" if unicode else "+")[0]
            if centered:
                for name, (ports, linked, title_start) in embedded.items():
                    upward = name == "judge"
                    border = 0 if upward else -1
                    edge = box_edge("", span, unicode) if upward else block[border]
                    branch = connections(width, root_top if upward else root_bottom, ports,
                                         [p for p, member in zip(ports, linked) if member["state"] in LIVE_STATES],
                                         frame, unicode, upward, 1)[0]
                    for absolute, glyph in enumerate(branch):
                        if glyph != " " and cursor < absolute < cursor + span - 1:
                            relative = absolute - cursor
                            edge = edge[:relative] + glyph + edge[relative + 1:]
                    if upward:
                        relative = title_start - cursor
                        caption = " " + title + " "
                        edge = edge[:relative] + caption + edge[relative + len(caption):]
                    block[border] = edge
            blocks.append(block)
            points.append(cursor + column)
            cursor += span + 1
        if before:
            before(points)
        start = len(lines) + 1
        for i, m in enumerate(group):
            for row in range(rows_h):
                hits.append({"y": start + row, "x1": left + i * (cell_width + 1) + 1,
                             "x2": left + i * (cell_width + 1) + cell_width, "action": action(m),
                             "preview": m.get("role") != "main"})
        for row in range(rows_h):
            value = " " * left
            flanks = centered and width >= 140 and rows_h >= 5
            if flanks:
                if row == 0:
                    left_width, right_width = left - 2, width - left - cell_width - 2
                    linked_workers = [m for m in linked_children if m.get("role") == "worker"]
                    status_rows = ["Workers %s / live %s" % (len(linked_workers), sum(m["state"] in LIVE_STATES for m in linked_workers)),
                                   "Advisors %s / reviewers %s" % (sum(m.get("role") == "advisor" for m in linked_children),
                                                                  sum(m.get("role") == "reviewer" for m in linked_children)),
                                   "Needs attention %s" % sum(m["state"] in ATTENTION_STATES for m in linked_children)]
                    ident = group[0]["participant"]
                    pending = sum(valid_count(mapping(p).get("pending")) or 0 for p in listing(room.get("pairs"))
                                  if mapping(p).get("recipient") == ident)
                    inbox = [m for m in listing(room.get("messages")) if ident in listing(m.get("targets"))]
                    mail = ["Pending %s / recent %s" % (pending, len(inbox))]
                    if inbox:
                        last = inbox[-1]
                        sender = next((m for m in members if m["participant"] == last.get("sender")), {})
                        sender_name = name_of(sender) if sender else last.get("sender", "unknown")
                        mail += ["From " + clean(sender_name)] + wrapped(clean(last.get("text")) or "", right_width - 4, rows_h - 4)
                    else:
                        mail += ["No addressed message recorded"]
                    left_card = card("WORK STATUS", status_rows, left_width, rows_h, unicode)
                    right_card = card("MAIL / to main", mail, right_width, rows_h, unicode)
                value = paint(left_card[row], "dim") + "  "
            for i, block in enumerate(blocks):
                cell = block[row]
                if color:
                    cell_style = tone(group[i]["state"]) if row == 1 and group[i]["state"] in ATTENTION_STATES else style
                    cell = ((PALETTE[cell_style] + "\033[7m" if action(group[i]) == selected else
                            ("\033[1m" if group[i].get("role") == "main" else "") + PALETTE[cell_style]) + cell + "\033[0m")
                value += cell + (" " if i < count - 1 else "")
            if flanks:
                value += "  " + paint(right_card[row], "alert" if pending else "dim")
            lines.append(value)
        return start, len(lines)

    def band(group, style, name, upward):
        def wire(points):
            if name in embedded:
                return
            if upward:
                points = [left + span // 2 + i * (span + 1) for i in range(len(group))]
            for line in connections(width, root_top if upward else root_bottom, points,
                                    [p for p, m in zip(points, group) if m["state"] in LIVE_STATES],
                                    frame, unicode, upward, wire_rows):
                add(line, style)
        span, left = layout(width, len(group))
        first, last = cards(group, style, top=not upward, bottom=upward, before=None if upward else wire)
        bands_hit.append({"name": name, "y1": first, "y2": last, "x1": 1, "x2": width})
        if upward:
            wire(None)

    def lane_view(rows):
        """One lane per main: its advisors and reviewers above it, one box per running worker below it."""
        per_row = grid_cols if grid else len(lanes)
        lane_width = (width - per_row + 1) // per_row
        if grid:
            # Row 2 starts below row 1's tallest lane; each row gets half the rows.
            for start in range(0, len(lanes), grid_cols):
                lane_row(lanes[start:start + grid_cols], rows // 2, lane_width)
        else:
            lane_row(lanes, rows, lane_width)

    def lane_row(group, rows, lane_width):
        first_band = len(bands_hit)
        tag_of = {"advisor": "Advisor", "reviewer": "Reviewer", "council": "Debate"}
        columns = []
        judge_sets = {m["participant"]: [c for c in teams[m["participant"]] if c.get("role") != "worker"][:3] for m in group}
        most = max(1, max(len(judges) for judges in judge_sets.values()))
        # With room, each main's advisors and reviewers share one box above it; boxes and mains line up across lanes.
        boxed = rows >= 14 and any(judge_sets.values())
        lift = most + 2 if boxed else most

        def activity_line(calls, m):
            ident = m["participant"]
            running = sum(c.get("role") == "worker" and c["state"] in LIVE_STATES for c in calls)
            needs = sum(c["state"] in ATTENTION_STATES for c in calls)
            reviews = len(grouped[ident]["review"])
            said_at = next((x.get("ts") for x in reversed(listing(room.get("messages"))) if x.get("sender") == ident), None)
            parts = ["%s worker%s running" % (running, "" if running == 1 else "s") if running else "no workers running"]
            if needs or reviews:
                what = [text for text in ("%s call%s" % (needs, "" if needs == 1 else "s") if needs else "",
                                          "%s review%s" % (reviews, "" if reviews == 1 else "s") if reviews else "") if text]
                parts.append(" + ".join(what) + (" needs you" if needs + reviews == 1 else " need you"))
            if ago(said_at):
                parts.append("last message " + ago(said_at))
            return " · ".join(parts)

        def lane_text(m, calls, rows_left):
            """Task, declared status and activity as [(line, dim)], wrapped into the rows the card has."""
            items = [(x, dim) for x, dim in ((claim_line(m), False), now_line(m)) if x]
            items = items or [("no status yet", True)]
            last = (activity_line(calls, m), False)
            kept = items[:max(1, rows_left - 1)] + [last] if rows_left >= 2 else items[:1]
            spare = rows_left - len(kept)
            span = [1] * len(kept)
            for n, (text, _) in enumerate(kept):
                while spare > 0 and span[n] < 3 and len(wrapped(text, lane_width - 4, 9)) > span[n]:
                    span[n] += 1
                    spare -= 1
            return [(line, kept[n][1]) for n in range(len(kept)) for line in wrapped(kept[n][0], lane_width - 4, span[n])]

        # The card shows its status line, then Task / Now / activity; the shown rows shrink on short boards.
        want = 6 if height >= 32 else 5 if height >= 24 else 4
        head = 3
        if rows >= 8:
            head = max(3, min(want, rows - lift - 3 - 1))
            if height >= 32:
                head = max(head, min(6, rows - lift - 1))
        for i, m in enumerate(group):
            x0 = i * (lane_width + 1)
            top = len(lines) + 1
            calls = teams[m["participant"]]
            judges_here = judge_sets[m["participant"]]
            workers_here = sorted((c for c in calls if c.get("role") == "worker"),
                                  key=lambda c: 0 if c["state"] in LIVE_STATES else 1 if c["state"] in ATTENTION_STATES else 2)
            cells = []

            def row_hit(offset, target):
                hits.append({"y": top + offset, "x1": x0 + 1, "x2": x0 + lane_width, "action": target, "preview": True})

            def judge_text(c):
                return "%s %s %s · %s · %s" % (activity(c["state"], frame, unicode), tag_of.get(c.get("role"), "Advisor"),
                                               name_of(c), call_span(c, "→"), task_of(c)) if unicode else \
                    "%s %s %s / %s / %s" % (activity(c["state"], frame, unicode), tag_of.get(c.get("role"), "Advisor"),
                                            name_of(c), call_span(c), task_of(c))

            def judge_style(c):
                return tone(c["state"]) if c["state"] in ATTENTION_STATES else "review"

            if boxed and judges_here:
                label = "ADVISORS / REVIEWS (%s)" % len(judges_here)
                box = card(label, [judge_text(c) for c in judges_here], lane_width, lift, unicode)
                box[-1] = joint(box[-1], 1, "┬" if unicode else "+")[0]
                for row, line in enumerate(box):
                    c = judges_here[row - 1] if 0 < row <= len(judges_here) else None
                    style = judge_style(c) if c else "review" if judges_here else "dim"
                    cells.append(chosen(line, style) if c and action(c) == selected else paint(line, style))
                    if c:
                        row_hit(len(cells) - 1, action(c))
            else:
                cells.extend([" " * lane_width] * (lift - max(1, len(judges_here))))
                if not judges_here:
                    # No judges, no box: one dim row keeps the lanes aligned and gives the rest to cards and workers.
                    cells.append(paint(padded(" ◇ Advisors: none active · a asks one" if unicode else
                                              " Advisors: none active / a asks one", lane_width), "dim"))
                for n, c in enumerate(judges_here):
                    text = padded(" %s─◇ %s" % ("┌" if n == 0 else "├", judge_text(c)) if unicode else
                                  " %s %s" % ("+" if n == 0 else "|", judge_text(c)), lane_width)
                    cells.append(chosen(text, judge_style(c)) if action(c) == selected else paint(text, judge_style(c)))
                    row_hit(len(cells) - 1, action(c))
            title = ("▸ " if unicode else "> ") if m == primary else ""
            status = "%s %s" % (activity(state_of(m), frame, unicode), said(state_of(m)))
            if open_count(room, m["participant"]):
                status += (" · " if unicode else " / ") + "? %s open" % open_count(room, m["participant"])
            ctx = context_note(report, m)
            if ctx:
                note = "ctx %s%%" % ctx[0] + (" · compact soon" if ctx[1] == "alert" and unicode else
                                             " / compact soon" if ctx[1] == "alert" else "")
                status += (" · " if unicode else " / ") + note
            texts = lane_text(m, calls, head - 3) if head > 3 else []
            content = [status] + [line for line, _ in texts] if head > 3 else [status + " · " + task_of(m)]
            block = card(title + "MAIN / " + name_of(m), content, lane_width, head, unicode)
            dim_rows = {2 + n for n, (_, dim) in enumerate(texts) if dim}
            block[0] = joint(block[0], 1, "┴" if unicode else "+")[0]
            block[-1] = joint(block[-1], 2, "┬" if unicode else "+")[0]
            style = hue(m)
            for row, line in enumerate(block):
                # The shown main's title edge reads as an active tab; its body keeps the plain colour.
                if row in dim_rows:
                    line = paint(line[0], style) + paint(line[1:-1], "dim") + paint(line[-1], style)
                    cells.append(line)
                elif row == 1 and ctx and note in line:
                    # Only the context reading takes its band's tone; the rest of the status line keeps the card's.
                    at = line.index(note)
                    cells.append(paint(line[:at], style) + paint(note, ctx[1] or style) + paint(line[at + len(note):], style))
                else:
                    cells.append(chosen(line, style) if m == primary and row == 0 else paint(line, style))
                hits.append({"y": top + lift + row, "x1": x0 + 1, "x2": x0 + lane_width,
                             "action": ("chat", m["participant"]), "select": True})
            key = "lane:" + m["participant"]
            capacity = max(0, (rows - len(cells) - 1) // 3)
            start = min(scrolled.get(key, 0), max(0, len(workers_here) - capacity))
            scrolled_now[key] = start
            shown = workers_here[start:start + capacity]
            hidden = len(workers_here) - len(shown)
            for n, c in enumerate(shown):
                last = n == len(shown) - 1 and not hidden
                box = card("Worker %s %s %s" % (name_of(c), activity(c["state"], frame, unicode),
                                                call_span(c, "→" if unicode else "->")),
                           [("round %s · " % c["round"] if c.get("round", 1) > 1 else "") + task_of(c)], lane_width - 3, 3, unicode)
                rails = (("└", " ", " ") if last else ("├", "│", "│")) if unicode else (("`", " ", " ") if last else ("|", "|", "|"))
                style = tone(c["state"]) if c["state"] in ATTENTION_STATES else "worker"
                for row, line in enumerate(box):
                    text = "  " + rails[row] + line
                    cells.append(chosen(text, style) if action(c) == selected else paint(text, style))
                    row_hit(len(cells) - 1, action(c))
            notes = []
            if hidden:
                running = sum(c["state"] in LIVE_STATES for c in workers_here[start + len(shown):] + workers_here[:start])
                notes.append("%s more%s · scroll" % (hidden, " (%s running)" % running if running else ""))
            if finished.get(m["participant"]):
                notes.append("Past work (%s)" % finished[m["participant"]])
            if not workers_here and capacity:
                cells.extend([paint(padded("  └ no workers" if unicode else "  ` no workers", lane_width), "dim")]
                             + [" " * lane_width] * 2)
            if notes:
                cells.append(paint(padded("  " + " / ".join(notes), lane_width), "dim"))
            bands_hit.append({"name": key, "y1": top, "y2": top + rows - 1, "x1": x0 + 1, "x2": x0 + lane_width})
            columns.append(cells)
        depth = max(len(cells) for cells in columns)
        for row in range(depth):
            lines.append(" ".join(cells[row] if row < len(cells) else " " * lane_width for cells in columns).rstrip())
        for band in bands_hit[first_band:]:
            band["y2"] = min(band["y2"], len(lines))

    if links and height >= 32:
        per_row = grid_cols if grid else len(lanes)
        lane_wide = (width - per_row + 1) // per_row if lanes else 0
        centres = {m["participant"]: i % per_row * (lane_wide + 1) + lane_wide // 2 for i, m in enumerate(lanes)}
        unread = sum(l["new"] for l in links)
        box = [box_edge("BETWEEN MAINS · %s pair%s · %s unread%s" % (
            len(links), "" if len(links) == 1 else "s", unread,
            " of %s in room" % unread_total if unread_total > unread else ""), width, unicode)]
        for n, link in enumerate(links[:4]):
            drawn = link_row(link, centres, labels, width, unicode)
            if drawn is None:
                drawn = box_row("%s%s%s  %s" % (labels[link["left"]], joiner, labels[link["right"]],
                                                 link_counts(link, unicode)), width, unicode)
            box.append(drawn)
            hits.append({"y": pair_at + 2 + n, "x1": 1, "x2": width, "preview": True,
                         "action": ("pair", (link["left"], link["right"]))})
        if len(links) > 4:
            box.append(box_row("+%s more pair%s" % (len(links) - 4, "" if len(links) == 5 else "s"), width, unicode))
        if latest:
            target = [t for t in listing(latest.get("targets")) if t in mains_by_id and t != latest["sender"]]
            stamp = clock_stamp(latest.get("ts"))
            box.append(box_row("last: %s → %s · %s%s" % (
                labels[latest["sender"]], ", ".join(labels[t] for t in target), stamp + " · " if stamp else "",
                clipped((readable(latest.get("text")).split("\n")[0]) or "(empty)", 60)), width, unicode))
        box.append(box_edge("", width, unicode, "bottom"))
        lines[pair_at:pair_at + box_height] = [
            paint(line, "dim" if latest and n == len(box) - 2 else "review") for n, line in enumerate(box)]
    scrolled_now = {"judge": judge_start, "worker": worker_start}
    main_width, main_left = layout(width, 1, True)
    root_bottom = main_left + main_width // 2
    root_top = root_bottom
    if lanes:
        lane_view(budget - used - reserve)
        for band in bands_hit:
            if band["name"].startswith("lane:"):
                band["y2"] = min(band["y2"], len(lines))
    else:
        if shown_judges:
            band(shown_judges, "review", "judge", True)
        elif primary and height >= 20:
            add(padded(" " * max(0, root_top - 10) + "◇ Advisors: none active · a asks one" if unicode else
                       " " * max(0, root_top - 10) + "Advisors: none active / a asks one", width), "dim")
        if primary:
            cards([primary], hue(primary), centered=True,
                  top=bool(shown_judges), bottom=bool(shown_workers))
        else:
            for line in card("MAIN", ["No main connected / start Codex or Claude"], width, card_height, unicode):
                add(line, "main")
        if shown_workers:
            band(shown_workers, "worker", "worker", False)
        elif primary and height >= 20:
            add(" " * max(0, root_bottom - 10) + ("▢ Workers: none running · they appear here when spawned" if unicode else
                                                   "Workers: none running / they appear here when spawned"), "dim")
    spare = budget - len(lines)
    if reserve and (automatic or previewed and spare > reserve):
        # Bands sized to their content leave rows the detail can use; keep one for the status line.
        reserve = max(reserve, spare - 1)
    body_drawn = False
    omitted = len(children) - len(shown_judges) - len(shown_workers) if not lanes else 0
    below = bool(omitted or unlinked)
    if reserve and spare >= reserve:
        # One box with tabs: the selected call's detail, the plan, this main's debates, the room's messages.
        from panel_debate import signature, tab_body as debate_body
        from panel_messages import tab_body as messages_body
        top = len(lines) + 1
        active = navigation.get("tab") if navigation.get("tab") in dict(TABS) else "detail"
        navigation["main_attempt"] = main_attempt
        body, targets = detail(previewed, preview, width - 4, members, room, teams, everyone, labels) if previewed else (
            ["No call selected · click a main or a call to see it here"], {})
        # A short detail does not stretch an empty box over the rest of the board; the other tabs need room too.
        room_rows = max(0, spare - below)
        # The box is the board's reading area: it takes every row the cards above leave, so a tab switch
        # never resizes it and long Messages or Plan tabs have room.
        rows_high = room_rows if room_rows >= 3 else max(min(reserve, max(3, len(body) + 3)), min(7, room_rows))
        strip_only = reserve < 3 and rows_high < 3
        rows_high = 1 if strip_only else rows_high
        capacity = max(1, rows_high - 2)
        seen = navigation.setdefault("debate_seen", {})
        shown_main = primary["participant"] if primary else None
        digest = signature(report, shown_main)
        if active == "debate":
            seen[shown_main] = digest
        marks = {"debate": (" •" if unicode else " *") if digest and seen.get(shown_main) != digest else "",
                 "messages": " %d unread" % unread_total if unread_total else ""}
        texts = [("[ %s ]" if key == active else "%s") % (name + marks.get(key, "")) for key, name in TABS]
        edge = box_edge("  ".join(texts), width, unicode)
        column, span = 3, None
        for key, text in zip(dict(TABS), texts):
            x1, x2 = column + 1, min(column + len(text), width - 3)
            if x1 <= x2:
                hits.append({"y": top, "x1": x1, "x2": x2, "action": ("tab", key)})
                span = (column, x2) if key == active else span
            column += len(text) + 2
        role = previewed.get("role", "worker").upper() if previewed else ""
        style = (hue(previewed) if role == "MAIN" else "review" if role != "WORKER" else "worker") if previewed else "dim"
        lines.append(paint(edge, style) if span is None or not color else
                     paint(edge[:span[0]], style) + chosen(edge[span[0]:span[1]], style) + paint(edge[span[1]:], style))
        # No room for a body: the strip alone keeps every tab one click away.
        if strip_only:
            navigation.pop("box", None)
            spare -= 1
        else:
            body_drawn = True
            first, plain = 0, set()
            if active == "detail" and capacity == 1 and body:
                # One row holds the content line itself; a title there would leave nothing to scroll.
                first = min(scrolled.get("detail", 0), len(body) - 1)
                scrolled_now["detail"] = first
                rows = [(body[first], targets.get(first, action(previewed)) if previewed else None, False)]
                plain = {0} if first not in targets else set()
            elif active == "detail":
                first = min(scrolled.get("detail", 0), max(0, len(body) - (capacity - 1)))
                scrolled_now["detail"] = first
                head = "%s / %s%s" % (role, name_of(previewed), " / auto" if automatic else "") if previewed else "DETAIL"
                if len(body) > capacity - 1:
                    head += " / %s-%s of %s / wheel" % (first + 1, min(len(body), first + capacity - 1), len(body))
                rows = [(head, action(previewed) if previewed else None, False)] + [
                    (text, targets.get(first + i, action(previewed)) if previewed else None, False)
                    for i, text in enumerate(body[first:first + capacity - 1])]
                plain = {i + 1 for i in range(capacity - 1) if first + i not in targets}
            elif active == "plan":
                plan = plan_rows(report, labels, width - 4, unicode)
                if capacity == 1 and len(plan) > 1:
                    plan.append(("Full plan: oms agent-plan list", None))
                page_size = max(1, capacity - int(len(plan) > capacity))
                first = min(scrolled.get("detail", 0), max(0, len(plan) - page_size))
                scrolled_now["detail"] = first
                rows = [(text, target, False) for text, target in plan[first:first + page_size]]
                if len(plan) > capacity:
                    notice = "%s rows hidden / wheel; full: oms agent-plan list" % (len(plan) - page_size)
                    if capacity > 1:
                        rows.insert(0, (notice, None, False))
                    else:
                        rows[0] = ("%s rows hidden / wheel | %s" % (len(plan) - page_size, rows[0][0]), None, False)
            elif active == "debate":
                rows = debate_body(report, shown_main, width - 4, capacity, navigation, labels, unicode)
                if navigation["debate_view"].get("shown") is None and not menu and not navigation.get("keys_help") \
                        and footer[-hint_rows:] == hints:
                    # Nothing to pick or open: the seat and target keys would do nothing.
                    footer[-hint_rows:] = hints = footer_hints(dict(navigation, debate_empty=True), width, managed,
                                                               unicode, chosen_member, len(mains), True)
            else:
                rows = messages_body(report, width - 4, capacity, navigation, unicode)
            for n in range(capacity):
                text, target, picked = rows[n] if n < len(rows) else ("", None, False)
                line = box_row(text, width, unicode)
                lines.append("\033[7m" + line + "\033[0m" if color and picked else line)
                if target:
                    # Body text without its own link is passive, so a click that selects text changes nothing.
                    hits.append({"y": top + 1 + n, "x1": 1, "x2": width, "action": target,
                                 **({"preview": True, "passive": n in plain} if active == "detail" else {})})
            lines.extend([box_row("", width, unicode)] * (rows_high - 2 - capacity))
            lines.append(paint(box_edge("", width, unicode, "bottom"), style))
            bands_hit.append({"name": "detail", "y1": top, "y2": top + rows_high - 1, "x1": 1, "x2": width, "step": 3})
            navigation["box"] = {"y1": top, "y2": top + rows_high - 1, "pages": bool(previewed) or active == "plan"}
            spare -= rows_high
    if spare and (omitted or unlinked):
        parts = ["%s more calls (scroll)" % omitted] if omitted else []
        open_unlinked = [m for m in unlinked if classify(m) != "past"]
        if unlinked:
            parts.append("%s call%s with no known main%s · t shows them" % (
                len(open_unlinked), "" if len(open_unlinked) == 1 else "s",
                " (+%s past)" % (len(unlinked) - len(open_unlinked)) if len(open_unlinked) < len(unlinked) else ""))
        add(" / ".join(parts), "alert" if open_unlinked else "dim")
        spare -= 1
    if not body_drawn and not menu and not navigation.get("keys_help") and footer[-hint_rows:] == hints:
        # Without a box body there is nothing to close or open in full.
        footer[-hint_rows:] = footer_hints(dict(navigation, preview=None, detail=None, full_result=None),
                                           width, managed, unicode, chosen_member, len(mains), True)
    lines += [""] * max(0, height - int(menu) - len(lines) - len(footer))
    lines += [clipped(line, width) for line in footer]
    if bar:
        row = len(lines) - (hint_rows + 1 if not menu else 1)
        lines[row] = paint(lines[row], "main" if bar[1] else "dim")
        hits += [{"y": row + 1, "x1": x1, "x2": x2, "action": ("spawn", name)} for x1, x2, name in bar[1]]
    visible = [m for lane in lanes for m in teams[lane["participant"]]] if lanes else shown_judges + shown_workers
    items = [("chat", m["participant"]) for m in mains] + [action(m) for m in pair_members]
    items += [action(m) for m in visible]
    items += [action(m) for calls in everyone.values() for m in calls if classify(m) == "live"] + group_actions
    items += [hit["action"] for hit in hits if hit["action"][0] in {"result", "debate"}]
    navigation.update(hits=hits, items=list(dict.fromkeys(items)), offset=offset, viewport=3,
                      body_rows=len(children), positions={item: i for i, item in enumerate(items)},
                      room_id=room.get("id"), graph_selected=selected, surface="graph",
                      primary=("chat", primary["participant"]) if primary else None,
                      mains=[m["participant"] for m in mains], bands=bands_hit, band_offsets=scrolled_now)
    text = "\n".join(lines)
    # Plain-word separators keep one-cell ASCII stand-ins, so hit columns do not move.
    return text if unicode else text.translate({ord("·"): "/", ord("→"): ">", ord("×"): "x", ord("│"): "|"})


TABS = (("detail", "Detail"), ("plan", "Plan"), ("debate", "Debate"), ("messages", "Messages"))
PLAN_ORDER = {"done": 0, "review": 1, "landing": 1, "running": 2, "claimed": 2, "ready": 3, "blocked": 4}


def plan_rows(report, labels, width, unicode):
    """The shared repository plan: its goal, then one row per task, verified work first."""
    goal = mapping(report.get("goal")).get("text")
    tasks = listing(mapping(report.get("room")).get("repo_tasks"))
    glyphs = (dict(done="✓", review="◐", landing="◐", running="●", claimed="●", ready="○", blocked="✗") if unicode else
              dict(done="+", review="~", landing="~", running="*", claimed="*", ready="o", blocked="x"))
    rows = [(line, None) for line in wrapped("Goal: " + goal, width, 2)] if goal else []
    for t in sorted((mapping(t) for t in tasks), key=lambda t: PLAN_ORDER.get(t.get("state"), 5)):
        by = t.get("claimed_by_participant")
        word = clean(t.get("state"), 20) or "unknown"
        age = t.get("claim_age_s")
        if t.get("claim_expired") and isinstance(age, (int, float)) and age >= 0:
            word += " %d%s stale" % ((age // 86400, "d") if age >= 86400 else (age // 3600, "h") if age >= 3600 else (age // 60, "m"))
        # A finished task has no owner or check left to act on, so those columns would only read as warnings.
        parts = [glyphs.get(t.get("state"), "?") + " " + clean(t.get("id"), 40), word, clean(t.get("title"), 120) or "untitled"]
        if t.get("state") != "done":
            parts += [clean(labels.get(by) or by, 40) if by else "unclaimed", clean(t.get("verify"), 80) or "no verify"]
        rows.append((" · ".join(parts), ("task", t.get("id")) if t.get("id") else None))
    return rows or [("No repository plan recorded", None)]


def action(member):
    return member.get("_action") or ("chat" if member.get("role") == "main" else "result", member["participant"])


def footer_hints(navigation, width, managed, unicode, member, mains, graph):
    """At most three hints for what is selected, then the fixed panel keys; `?` swaps in the full list of working keys."""
    selected = navigation.get("selected")
    kind = selected[0] if selected else "chat"
    if mapping(navigation.get("preview")).get("target", ("",))[0] == "pair":
        # The open detail is a link between two mains; chatting or asking would address neither.
        kind = "pair"
    arrows, move = ("←→", "↑↓") if unicode else ("<>", "^v")
    toggle = "F9 Chat⇄Board" if unicode else "F9 Chat<>Board"
    worktree = bool(member and kind == "result" and str(member.get("location") or "").startswith("worktree:"))
    if navigation.get("keys_help"):
        keys = ["Enter Open", move + " Move", arrows + " Main" if mains > 1 else "",
                "Space " + ("Pin" if graph else "Fold") if mains else "", "a Ask advisor",
                "w Worktree" if worktree else "", "f Full result" if navigation.get("full_result") else "",
                "Tab Next tab" if graph else "", "g Graph", "t Tree", "n New main" if managed else "",
                "v " + ("Collapse" if navigation.get("expanded") else "Expand"), "b Attention", "Esc Back",
                "Shift-drag Copy" if managed else "", "F6/F7 Main" if managed else "", toggle if managed else "q Quit", "? Hide"]
        lines = [""]
        for key in [k for k in keys if k]:
            if lines[-1] and display_width(lines[-1] + "  " + key) > width:
                lines.append("")
            lines[-1] += ("  " if lines[-1] else "") + key
        return lines[:4] + (["Tab badges: W workers  A advisors  ! needs you  M mail  ? open questions  c context left"] if graph else [])
    tab = navigation.get("tab") if graph and not navigation.get("detail") else None
    contextual = ([move + " Message", "Enter Open", arrows + " Filter"] if tab == "messages"
                  else [move + " Seat", "Enter Open", arrows + " Target"] if tab == "debate" and not navigation.get("debate_empty")
                  else ["Tab Next tab"] if tab == "debate"
                  else ["wheel Scroll", "Tab Next tab"] if tab == "plan" else None)
    contextual = contextual or (["Esc Back to graph", "g Graph"] if navigation.get("group_tree") else []) + ["f Full result" if navigation.get("full_result") else "",
                  "Esc Close" if navigation.get("preview") or navigation.get("detail") else "",
                  "Enter Chat" if kind == "chat" else "Enter Fold" if kind == "group" else "" if kind == "pair" else "Enter Show",
                  "a Ask advisor" if kind == "chat" else "w Worktree" if worktree else "",
                  "Tab Next tab" if graph and not navigation.get("detail") else "",
                  ("Space Pin" if graph else "Space Fold") if kind == "chat" and (mains > 1 or not graph) else "Space Fold" if kind == "group" else ""]
    # The panel-wide F-keys stay fixed at the end of every board; board keys fill what is left.
    control = navigation.get("control_window", True)
    reserved = ["F6/F7 Main", toggle] + (["F5 Control"] if control else []) + ["F12 Keys"] if managed else ["q Quit", "? Keys"]
    packed = []
    tail = "  ".join(reserved)
    for hint in [h for h in contextual if h]:
        if len(packed) < 3 and display_width("  ".join(packed + [hint] + reserved)) <= width:
            packed.append(hint)
    if display_width(tail) > width and managed:
        # Narrow boards keep every panel key visible in short form.
        reserved = ["F6/F7 F9 F5" if control else "F6/F7 F9", "F12 Keys"]
        tail = "  ".join(reserved)
        packed = []
    return ["  ".join(packed + reserved)] if display_width(tail) <= width else [clipped(tail, width)]
