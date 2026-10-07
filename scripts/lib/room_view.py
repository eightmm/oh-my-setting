"""Room graph drawn from participation, addressed messages and lifecycle evidence."""

import calendar
import re
import time

from dashboard_projection import clean, count as valid_count, display_width, fit, listing, mapping
from panel_view import (ATTENTION_STATES, MODEL_NAMES, LIVE_STATES, PALETTE, activity, box_edge, box_row,
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
    for member in listing(room.get("participants")):
        if not member.get("joined"):
            continue
        # Dispatched children inherit their main's participant as provenance;
        # only a same-role attempt is the member itself.
        attempt = next((row for row in linked.get(member.get("participant"), [])
                        if mapping(row.get("panel")).get("role") == member.get("role")), {})
        meta = mapping(attempt.get("panel"))
        output.append(dict(member, attempt=attempt.get("attempt_id"), state=attempt.get("state") or "presence unknown",
                           model=meta.get("model") or member.get("model"),
                           task_id=attempt.get("task_id"), location=meta.get("location"),
                           effort=meta.get("effort"), access=meta.get("access"),
                           title=meta.get("label") or member.get("label")))
    rank = lambda member: (0 if member["state"] in LIVE_STATES else 2 if member["state"] in {"done", "cancelled"} else 1, -member.get("seq", 0))
    return sorted(output, key=rank)


def window_order(report, mains):
    """Mains in tmux window order, the order F6/F7 step through; join order where no window is known."""
    windows = mapping(report.get("main_windows"))
    slot = lambda m: windows.get(m["participant"]) if type(windows.get(m["participant"])) is int else float("inf")
    return sorted(mains, key=lambda m: (slot(m), m.get("seq", 0), m["participant"]))


def model_name(member):
    return MODEL_NAMES.get(member.get("model"), member.get("model")) or member.get("provider", "unknown")


def main_names(report, members):
    """Display name per main: the model, plus its tmux window index (else the id tail) when another main shares it."""
    mains = [m for m in members if m.get("role") == "main"]
    windows = mapping(report.get("main_windows"))
    plain = [model_name(m) for m in mains]
    names = {}
    for m, name in zip(mains, plain):
        slot = windows.get(m["participant"])
        names[m["participant"]] = name if plain.count(name) == 1 else "%s #%s" % (
            name, slot if type(slot) is int else m["participant"][-4:])
    return names


def settler(report):
    """(settled, aged) for calls: finished, aged-out or handled calls leave the board; shared with the tree."""
    room = mapping(report.get("room"))
    # Only the call's own result handoff, read by its main, counts as a handled failure.
    read_results = {mapping(m).get("id") for m in listing(room.get("messages"))
                    if str(mapping(m).get("id", "")).startswith("result-") and not listing(mapping(m).get("pending_for"))}
    finalized = mapping(report.get("finalized"))
    attempts_seen = mapping(report.get("attempts"))
    # Only a complete active list proves a missing call has ended; the projection keeps at most 8 active rows.
    complete = attempts_seen.get("available") is True and valid_count(attempts_seen.get("active")) is not None and (
        valid_count(attempts_seen.get("active")) <= len(listing(attempts_seen.get("active_recent"))))

    def aged(call):
        # A call missing from a complete active list for ten minutes has ended.
        return complete and (joined_seconds(call) or 0) > 600

    def settled(call):
        # Finished or aged-out calls, and failed ones whose result its main has read or finalized, leave the board.
        return call["state"] in FINISHED or call["state"] == "presence unknown" and aged(call) or (
            call["state"] in ATTENTION_STATES - {"waiting_input", "waiting_approval", "review", "blocked"}
                                             and ("result-" + str(call.get("participant")) in read_results
                                                  or finalized.get(call.get("task_id")) in {"completed", "accepted"}))
    return settled, aged


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
    reading = LIVE_STATES | {"waiting_input", "waiting_approval", "review", "blocked"}
    ids = {m["participant"] for m in mains} | {m["participant"] for m in members if m.get("role") != "main"
                                              and m.get("parent") in owners and m.get("state") in reading
                                              and not settled(m)}
    return sum(valid_count(mapping(p).get("pending")) or 0
               for p in listing(mapping(report.get("room")).get("pairs")) if mapping(p).get("recipient") in ids)


def plan_idle(report):
    return valid_count(mapping(report.get("plan")).get("idle_days"))


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


def readable(text, joiner="\n"):
    """Text for people: machine status lines and markdown marks are dropped, call exits become words."""
    output = []
    for line in str(text or "").splitlines():
        line = line.strip()
        exited = re.match(r"Call exit=(\d+); parent acceptance pending\.?\s*(.*)", line)
        if exited:
            line = (("Finished. " if exited.group(1) == "0" else "Failed (exit %s). " % exited.group(1)) + exited.group(2)).strip()
        if not line or MACHINE.match(line):
            continue
        line = re.sub(r"\*\*|__|`", "", re.sub(r"^#+\s*", "", line))
        output.append(line)
    return joiner.join(output)


def declared_status(room, participant, width):
    """A main's own declared task as "Now: text (HH:MM)" lines; the text is peer data."""
    found = (room.get("statuses") or {}).get(participant) or {}
    text = readable(found.get("text"), " ")
    try:
        stamp = time.strftime("%H:%M", time.localtime(calendar.timegm(time.strptime(str(found.get("ts")), "%Y-%m-%dT%H:%M:%SZ"))))
    except ValueError:
        stamp = ""
    return wrapped("Now: " + text + (" (" + stamp + ")" if stamp else ""), width, 2) if text else []


def detail(member, preview, width, members=(), room=None, teams=None, everyone=None, labels=None):
    """Selected block content in plain words; reading mail here never acknowledges it."""
    room = room or {}
    names = {m["participant"]: who(m, labels) for m in members}
    name = mapping(labels).get(member.get("participant")) or model_name(member)
    state = "exited" if member.get("role") == "main" and member.get("state") == "done" else member.get("state")
    title = clean(member.get("title")) or ""
    title = "" if title in ("Native task not recorded", "Task unrecorded") else title
    where = "in the repository" if member.get("location") in (None, "repository") else "in its own worktree"
    targets = {}
    if member.get("role") == "main":
        lines = wrapped("%s · %s main · %s%s" % (name, (member.get("provider") or "").capitalize(), said(state),
                                                 " · " + title if title else " · no task title yet"), width, 3)
        inbox = [m for m in listing(room.get("messages")) if member["participant"] in listing(m.get("targets"))]
        unread = sum(member["participant"] in listing(m.get("pending_for")) for m in inbox)
        lines += declared_status(room, member["participant"], width)
        lines.append("Messages to %s: %s%s" % (who(member, labels), len(inbox), " (%s unread)" % unread if unread else ""))
        for m in inbox[-3:]:
            lines += wrapped("  From %s: %s" % (names.get(m.get("sender"), clean(m.get("sender"))),
                                                readable(m.get("text"), " ") or "(empty)"), width, 2)
        calls = (everyone or teams or {}).get(member["participant"], [])
        running = [c for c in calls if c["state"] not in FINISHED]
        lines.append("Team: %s advisor%s, %s worker%s%s" % (
            sum(c.get("role") != "worker" for c in calls), "" if sum(c.get("role") != "worker" for c in calls) == 1 else "s",
            sum(c.get("role") == "worker" for c in calls), "" if sum(c.get("role") == "worker" for c in calls) == 1 else "s",
            " · %s finished" % (len(calls) - len(running)) if len(calls) > len(running) else ""))
        for c in calls:
            label = MODEL_NAMES.get(c.get("model"), c.get("model")) or "unknown"
            # Team rows are click targets, so finished calls stay reachable from their main.
            for part in wrapped("  %s%s %s · %s · %s" % ("! " if c["state"] in ATTENTION_STATES else "", ROLES.get(c.get("role"), "Worker"),
                                                       label, said(c["state"]), clean(c.get("title")) or "no task title"), width, 2):
                targets[len(lines)] = action(c)
                lines.append(part)
        native = next((c for c in calls if c.get("_native")), None)
        if native:
            lines.append("Built-in advisor: " + ("asked %s · running" % native["_native"] if native["state"] != "done"
                                                 else "last answered %s" % (native.get("_finished") or native["_native"])))
        lines.append("Works %s%s" % (where, " · effort " + member["effort"] if member.get("effort") else ""))
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
    access = "can edit" if member.get("access") == "write" else "read-only"
    lines.append("%s %s%s" % (access.capitalize(), where, " · effort " + member["effort"] if member.get("effort") else ""))
    loaded = preview.get("report")
    if preview.get("error"):
        return lines + [readable(preview["error"], " ")], targets
    if loaded is None:
        return lines + ["Loading its recorded result..."], targets
    if not listing(loaded.get("rows")):
        return lines + ["No result recorded yet (%s)" % said(state)], targets
    row = listing(loaded.get("rows"))[0]
    answer = readable(latest_answer(loaded))
    outcome = {"accepted": "Accepted by its main", "completed": "Completed; the main recorded a summary",
               "failed": "Marked failed by its main"}.get(row.get("outcome"), "Not reviewed by its main yet")
    check = mapping(row.get("verification")).get("status")
    lines.append(outcome + " · " + ("checks %s" % check if check else "no checks run"))
    if answer:
        lines += ["Answer:"] + [part for paragraph in answer.splitlines() if paragraph.strip()
                                for part in wrapped("  " + paragraph, width, 40, max_chars=4000)]
    changes = next((c["changes"] for c in reversed(listing(row.get("calls"))) if c.get("changes")), None)
    if changes and changes.get("issue"):
        lines.append("Changed files: unavailable (" + changes["issue"] + ")")
    elif changes and changes.get("count"):
        lines.append("Changed files: %s (+%s -%s)" % (changes["count"], changes["added"], changes["removed"]))
        lines += ["  %s +%s -%s" % tuple(f) for f in changes["files"][:12]]
        lines += ["  and %s more files" % (changes["count"] - 12)] if changes["count"] > 12 else []
    summary = readable(row.get("summary"))
    if summary and summary != answer:
        lines += ["Main's summary:"] + [part for paragraph in summary.splitlines() if paragraph.strip()
                                        for part in wrapped("  " + paragraph, width, 8)]
    if str(member.get("location") or "").startswith("worktree:") and member.get("state") in FINISHED | {"failed"}:
        lines.append("w opens its worktree in a shell")
    return lines, targets


FINISHED = {"done", "cancelled"}


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
    from panel_metrics import header_rows
    from dashboard_projection import use_unicode
    use_unicode(unicode)
    interactive = navigation is not None
    navigation = navigation if interactive else {}
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
        return sorted(calls, key=lambda m: (m.get("role") == "worker", m["state"] not in ATTENTION_STATES))

    teams = {m["participant"]: team(m) for m in mains}
    linked_children = team(primary, every=True) if primary else []
    # Finished calls leave the board (results stay in the main's detail and the tree); attention stays.
    settled, _ = settler(report)
    finished = {key: sum(settled(c) for c in calls) for key, calls in teams.items()}
    everyone = teams
    teams = {key: [c for c in calls if not settled(c)] for key, calls in teams.items()}
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
    if (pins is None and (not main_attempt or navigation.get("expanded") or navigation.get("overview"))
            and 2 <= len(mains) <= width // 30):
        # The live board, the control window or an expanded board overviews every main that fits.
        pins = [m["participant"] for m in mains]
    navigation["auto_pins"] = pins if navigation.get("pinned") is None else None
    pinned = [m for m in mains if m["participant"] in (pins or ())]
    lanes = pinned[:width // 30] if len(pinned) >= 2 and height >= 20 else []
    if lanes and primary in mains and primary not in lanes:
        # The selected main always has a lane, so the tab marker never points at an undrawn main.
        lanes = [m for m in mains if m in lanes[:width // 30 - 1] or m is primary]
    preview = mapping(navigation.get("preview"))
    previewed = next((m for m in mains + [c for calls in everyone.values() for c in calls]
                      if action(m) == preview.get("target")), None)
    if menu:
        footer = ["[1] Codex  [2] Claude  [h] Chats  [?] More  [q] " + ("Detach" if managed else "Quit")]
    else:
        # Whole hint fragments in priority order; the quit/return key always stays visible.
        quit_hint = "q " + ("Chat" if managed else "Quit")
        from panel_debate import debates as room_debates
        has_debates = bool(room_debates(report))
        hints = ["Click: show below", "F6/F7 Main" if managed else "", ("^v" if not unicode else "↑↓") + " Move",
                 ("<>" if not unicode else "←→") + " Main" if len(mains) > 1 else "", "Enter Chat", "a Ask advisor",
                 "w Worktree" if previewed and str(previewed.get("location") or "").startswith("worktree:") else "",
                 "Space Pin" if len(mains) > 1 else "", "v Expand", "t Tree",
                 "d Debates" if has_debates else "", "Esc Back"]
        packed = ""
        for hint in [h for h in hints if h]:
            if display_width(packed + hint + "  " + quit_hint) <= width:
                packed += hint + "  "
        footer = [packed + quit_hint]
    if navigation.get("notice"):
        if height >= 20 or menu:
            footer.append(clean(navigation["notice"], 180))
        else:
            # A short board shows the notice in place of the hints, never silently.
            footer = [clipped(clean(navigation["notice"], 180), max(1, width - display_width(quit_hint) - 2)) + "  " + quit_hint]
    publications = mapping(room.get("publications"))
    if publications:
        footer.append("APP DELIVERY / " + " | ".join("%s: %s%s" % (
            target, mapping(receipt).get("status", "unknown"),
            " (unconfirmed)" if mapping(receipt).get("delivery_unknown") else "")
            for target, receipt in list(publications.items())[-3:]))
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

    def task_of(m):
        task = clean(m.get("title")) or "Task unrecorded"
        if task not in ("Native task not recorded", "Task unrecorded"):
            return task
        if m.get("role") == "main":
            declared = readable((room.get("statuses") or {}).get(m["participant"], {}).get("text"), " ")
            if declared:
                return "Now: " + declared
            # Native task text is never intercepted; the main's own latest message is its visible status.
            said_last = next((readable(x.get("text"), " ") for x in reversed(listing(room.get("messages")))
                              if x.get("sender") == m["participant"] and readable(x.get("text"), " ")), "")
            if said_last:
                return "Latest sent: " + said_last
        return "no task title yet"

    # Mail to finished calls is never read again; unread mail to mains and running calls is counted.
    unread_total = live_unread(report, members, settled)
    heading_text = "OMS · " + (clean(room.get("title") or room.get("id")) or "Work room") + " · %s main%s · %s" % (
        len(mains), "" if len(mains) == 1 else "s",
        "%s unread for live participants" % unread_total if unread_total else "no unread messages")
    clock = time.strftime("%H:%M")
    if display_width(heading_text) + len(clock) + 2 <= width:
        heading_text += " " * (width - display_width(heading_text) - len(clock)) + clock
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
        multiple = len(mains) > 1
        if multiple:
            every = len(pinned) == len(mains)
            text = ("[x All] " if every else "[+ All] ")
            hits.append({"y": y, "x1": 1, "x2": len(text) - 1, "action": ("pin", "*")})
            value, x = paint(text, "dim"), len(text)
        room_width = width - x - 14  # "◂ " plus the widest "+NN ▸ !NN" marker
        per_page = max(1, min(len(mains), room_width // 24))
        tab_width = min(44, room_width // per_page)
        first = mains.index(primary) // per_page * per_page if primary in mains else 0
        if first:
            hits.append({"y": y, "x1": x + 1, "x2": x + 1, "action": ("chat", mains[first - 1]["participant"]),
                         "select": True})
            value, x = value + "◂ " if unicode else value + "< ", x + 2
        for m in mains[first:first + per_page]:
            calls = teams[m["participant"]]
            title = clean(m.get("title"))
            nickname = m["participant"][-4:] if not title or title == "Native task not recorded" else title
            alerts = sum(c["state"] in ATTENTION_STATES for c in calls)
            # Unread mail addressed to this main, including messages from other mains.
            unread = sum(valid_count(mapping(p).get("pending")) or 0 for p in listing(room.get("pairs"))
                         if mapping(p).get("recipient") == m["participant"])
            badge = " ".join("%s%s" % (mark, n) for mark, n in (
                ("W", sum(c.get("role") == "worker" for c in calls)),
                ("A", sum(c.get("role") != "worker" for c in calls)), ("!", alerts), ("M", unread)) if n)
            pin = ("[x]" if m in pinned else "[+]") if multiple else ""
            mark = ("▸" if unicode else ">") if m == primary else " "
            room_left = max(4, tab_width - (len(badge) + 1 if badge else 0) - len(pin) - 2)
            lead, model = "%s%s " % (mark, activity(state_of(m), frame, unicode)), name_of(m)
            label = lead + model + ("·" if unicode else "/") + nickname
            if display_width(label) > room_left:
                # The tail tells mains apart; keep it and shorten the shared model prefix first.
                left = room_left - display_width(lead) - 1
                keep = min(display_width(nickname), max(6, left - display_width(model)))
                label = lead + (fit(model, max(1, left - keep)) if left - keep < display_width(model) else model) + (
                    "·" if unicode else "/") + tail(nickname, keep)
            body = padded(label, room_left) + (" " + badge if badge else "") + " "
            hits.append({"y": y, "x1": x + 1, "x2": x + display_width(body), "action": ("chat", m["participant"]),
                         "select": True})
            if pin:
                hits.append({"y": y, "x1": x + display_width(body) + 1, "x2": x + display_width(body) + 3,
                             "action": ("pin", m["participant"])})
            style = "alert" if alerts else hue(m)
            cell = (chosen(body, style) if m == primary else paint(body, style)) + paint(pin, "dim")
            value += cell + " "
            x += display_width(body) + len(pin) + 1
        rest = len(mains) - first - per_page
        if rest > 0:
            hidden = sum(c["state"] in ATTENTION_STATES for m in mains[first + per_page:] for c in teams[m["participant"]])
            marker = "+%s %s" % (rest, "▸" if unicode else ">") + (" !%s" % hidden if hidden else "")
            hits.append({"y": y, "x1": x + 1, "x2": min(width, x + len(marker)), "select": True,
                         "action": ("chat", mains[first + per_page]["participant"])})
            value += paint(marker, "alert" if hidden else "dim")
        lines.append(value)
    peers = {m["participant"]: name_of(m) for m in mains}
    pairs = [mapping(p) for p in listing(room.get("pairs"))]
    spoken = listing(room.get("messages"))
    between = [(p, i) for i, p in enumerate(pairs) if p.get("sender") in peers and p.get("recipient") in peers]
    if between and height >= 20:
        # Mains call on each other through room messages; unread pairs lead, then the most recent.
        def recent(p):
            return max((i for i, x in enumerate(spoken) if mapping(x).get("sender") == p.get("sender")
                        and p.get("recipient") in listing(mapping(x).get("targets"))), default=-1)
        between = [p for p, i in sorted(between, key=lambda t: (
            not valid_count(t[0].get("pending")), -recent(t[0]), -t[1]))]
        unread_pairs = sum(valid_count(p.get("pending")) or 0 for p in between)
        if height < 32:
            add("Between mains: %s pair%s · %s unread" % (len(between), "" if len(between) == 1 else "s", unread_pairs),
                "review")
        else:
            add("Between mains", "review")
            for p in between[:4]:
                sent, pending = valid_count(p.get("sent")) or 0, valid_count(p.get("pending"))
                add("  %s → %s · %s message%s%s" % (
                    peers[p["sender"]], peers[p["recipient"]], sent, "" if sent == 1 else "s",
                    " · %s unread" % pending if pending else ""), "review")
            if len(between) > 4:
                add("  +%s more pair%s" % (len(between) - 4, "" if len(between) == 5 else "s"), "dim")
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
        return 4 + max(len(wrapped(task_of(m), span - 4, 2)) for m in group) if compact(group, span) else 3

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
        return budget - used - extra >= 6 if lanes else budget - used - wires - extra >= minimum_total

    def detail_rows():
        # The overview keeps one-line cards so the selected block's detail gets the rest.
        if not previewed:
            return 0
        room_left = budget - used - (max(6, (budget - used) // 2) if lanes else wires + minimum_total) - 1
        return room_left if room_left >= 5 else 3 if fits(3) else 0

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
        return max(3 + extras(m) + reflow + len(wrapped(task_of(m), span - 4, 2)) for m in group)

    # Cards never keep blank rows; rows they do not need go to the detail area.
    def drawn_rows(card_height):
        return sum(max(minimum_rows(g), min(card_height, needed(g, g == [primary]))) for g in shown_groups)

    available_cards = budget - used - wires - reserve
    if (interactive and not previewed and primary and not lanes and height >= 24
            and not navigation.get("dismissed") and available_cards - minimum_total >= 6):
        available_cards -= 6  # Five detail rows and one status row.
    card_height = next((candidate for candidate in range(6, 2, -1)
                        if drawn_rows(candidate) <= available_cards), 3)
    if reserve > 3 and height < 9 * band_count + 20:
        card_height, reserve = 3, budget - used - wires - (3 * band_count if lanes else minimum_total) - 1
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
            if m.get("role") != "main" and primary:
                status += " · for " + clean(name_of(primary))
            task = task_of(m)
            content = [status + " / " + task] if rows_h == 3 else [status] + wrapped(task, span - 4, rows_h - 3)
            if rows_h >= 4:
                where = " / ".join(v for v in ("@ " + location_label(m["location"]) if m.get("location") else "",
                                                m.get("effort") or "") if v)
                if m.get("role") == "main":
                    calls = teams.get(m["participant"], [])
                    where = "Team W%s A%s%s" % (
                        sum(c.get("role") == "worker" for c in calls), sum(c.get("role") != "worker" for c in calls),
                        " / !%s attention" % sum(c["state"] in ATTENTION_STATES for c in calls)
                        if any(c["state"] in ATTENTION_STATES for c in calls) else "")
                task_rows = len(wrapped(task, span - 4, 2))
                tail = [where] if where and rows_h >= 4 + task_rows + int(reflow) else []
                content = ([full_title(m)] if reflow else []) + [status] + wrapped(
                    task, span - 4, max(1, min(2, rows_h - 3 - len(tail) - int(reflow)))) + tail
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
        count = len(lanes)
        lane_width = (width - count + 1) // count
        tag_of = {"advisor": "Advisor", "reviewer": "Reviewer", "council": "Debate"}
        columns = []
        judge_sets = {m["participant"]: [c for c in teams[m["participant"]] if c.get("role") != "worker"][:3] for m in lanes}
        most = max(1, max(len(judges) for judges in judge_sets.values()))
        # With room, each main's advisors and reviewers share one box above it; boxes and mains line up across lanes.
        boxed = rows >= 16
        lift = most + 2 if boxed else most

        def doing(m):
            # The recorded task, then the main's latest message: what it is doing now.
            lines = wrapped(task_of(m), lane_width - 4, 3)
            latest = task_of(dict(m, title=None))
            if len(lines) < 3 and latest.startswith("Latest sent: ") and latest != task_of(m):
                lines += wrapped(latest, lane_width - 4, 3 - len(lines))
            return lines

        # Spare rows grow every main card alike, up to three task lines, keeping one worker box below.
        body = max(1, min(3, rows - lift - 3 - 3 - 1)) if rows >= 12 else 0
        head = 3 + body if body else 3
        for i, m in enumerate(lanes):
            x0 = i * (lane_width + 1)
            top = len(lines) + 1
            calls = teams[m["participant"]]
            judges_here = judge_sets[m["participant"]]
            workers_here = [c for c in calls if c.get("role") == "worker"]
            cells = []

            def row_hit(offset, target):
                hits.append({"y": top + offset, "x1": x0 + 1, "x2": x0 + lane_width, "action": target, "preview": True})

            def judge_text(c):
                return "%s %s %s · %s · %s" % (activity(c["state"], frame, unicode), tag_of.get(c.get("role"), "Advisor"),
                                               name_of(c), said(c["state"]), task_of(c)) if unicode else \
                    "%s %s %s / %s / %s" % (activity(c["state"], frame, unicode), tag_of.get(c.get("role"), "Advisor"),
                                            name_of(c), said(c["state"]), task_of(c))

            def judge_style(c):
                return tone(c["state"]) if c["state"] in ATTENTION_STATES else "review"

            if boxed:
                label = "ADVISORS / REVIEWS" + (" (%s)" % len(judges_here) if judges_here else " / none active")
                box = card(label, [judge_text(c) for c in judges_here] or ["—" if unicode else "-"], lane_width, lift, unicode)
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
                    # The footer already says how to ask; an empty place keeps its row, not its sentence.
                    cells.append(paint(padded(" ┌─◇ —" if unicode else " + -", lane_width), "dim"))
                for n, c in enumerate(judges_here):
                    text = padded(" %s─◇ %s" % ("┌" if n == 0 else "├", judge_text(c)) if unicode else
                                  " %s %s" % ("+" if n == 0 else "|", judge_text(c)), lane_width)
                    cells.append(chosen(text, judge_style(c)) if action(c) == selected else paint(text, judge_style(c)))
                    row_hit(len(cells) - 1, action(c))
            title = ("▸ " if unicode else "> ") if m == primary else ""
            status = "%s %s" % (activity(state_of(m), frame, unicode), said(state_of(m)))
            content = [status] + doing(m)[:body] if body else [status + " · " + task_of(m)]
            block = card(title + "MAIN / " + name_of(m), content, lane_width, head, unicode)
            block[0] = joint(block[0], 1, "┴" if unicode else "+")[0]
            block[-1] = joint(block[-1], 2, "┬" if unicode else "+")[0]
            style = hue(m)
            for row, line in enumerate(block):
                # The shown main's title edge reads as an active tab; its body keeps the plain colour.
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
                box = card("Worker %s %s %s" % (name_of(c), activity(c["state"], frame, unicode), said(c["state"])),
                           [task_of(c)], lane_width - 3, 3, unicode)
                rails = (("└", " ", " ") if last else ("├", "│", "│")) if unicode else (("`", " ", " ") if last else ("|", "|", "|"))
                style = tone(c["state"]) if c["state"] in ATTENTION_STATES else "worker"
                for row, line in enumerate(box):
                    text = "  " + rails[row] + line
                    cells.append(chosen(text, style) if action(c) == selected else paint(text, style))
                    row_hit(len(cells) - 1, action(c))
            notes = []
            if hidden:
                notes.append("%s more workers (scroll)" % hidden)
            if finished.get(m["participant"]):
                notes.append("%s %s done" % ("✓" if unicode else "+", finished[m["participant"]]))
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

    scrolled_now = {"judge": judge_start, "worker": worker_start}
    main_width, main_left = layout(width, 1, True)
    root_bottom = main_left + main_width // 2
    root_top = root_bottom
    if lanes:
        lane_view(budget - used - reserve)
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
    if reserve and spare >= reserve:
        top = len(lines) + 1
        body, targets = detail(previewed, preview, width - 4, members, room, teams, everyone, labels)
        # A short detail does not stretch an empty box over the rest of the board.
        reserve = min(reserve, max(3, len(body) + 2))
        capacity = max(1, reserve - 2)
        first = min(scrolled.get("detail", 0), max(0, len(body) - capacity))
        scrolled_now["detail"] = first
        role = previewed.get("role", "worker").upper()
        title = "DETAIL / %s / %s%s" % (role, name_of(previewed), " / auto" if automatic else "")
        if len(body) > capacity:
            title += " / %s-%s of %s / wheel" % (first + 1, min(len(body), first + capacity), len(body))
        shown = body[first:first + capacity] if reserve > 3 else body[:1]
        for line in card(title, shown, width, reserve, unicode):
            add(line, hue(previewed) if role == "MAIN" else "review" if role != "WORKER" else "worker")
        for row in range(reserve):
            line = first + row - 1  # border row 0; body rows follow
            hits.append({"y": top + row, "x1": 1, "x2": width, "preview": True,
                         "action": targets.get(line, action(previewed)) if 0 < row < reserve - 1 else action(previewed)})
        bands_hit.append({"name": "detail", "y1": top, "y2": top + reserve - 1, "x1": 1, "x2": width, "step": 3})
        spare -= reserve
    omitted = len(children) - len(shown_judges) - len(shown_workers) if not lanes else 0
    if spare and (omitted or unlinked):
        parts = ["%s more calls (scroll)" % omitted] if omitted else []
        if unlinked:
            parts.append("%s call%s with no known main · t shows them" % (len(unlinked), "" if len(unlinked) == 1 else "s"))
        add(" / ".join(parts), "alert" if unlinked else "dim")
        spare -= 1
    acceptance = mapping(report.get("acceptance"))
    if spare and acceptance.get("available") and (plan_idle(report) or 0) < 7:
        add("Plan: %s of %s tasks verified" % (mapping(acceptance.get("counts")).get("verified", 0),
            acceptance.get("total", "?")), "good" if acceptance.get("complete") else "alert")
        spare -= 1
    messages = listing(room.get("messages"))
    if spare >= 3 and messages:
        def person(ident):
            return "everyone" if ident == "all" else who(next((m for m in members if m["participant"] == ident), None), labels)
        logs = [clipped("%s → %s: %s" % (person(m.get("sender")), person((listing(m.get("targets")) or ["all"])[0]),
                                        readable(m.get("text"), " ") or "(empty)"), width - 4)
                for m in messages[-min(3, spare - 2):]]
        for line in card("RECENT MESSAGES", logs, width, len(logs) + 2, unicode):
            add(line, "dim")
    elif spare >= 3 and listing(room.get("repo_tasks")):
        tasks = listing(room.get("repo_tasks"))
        body = ["%s · %s" % (t.get("title") or t.get("id"), said(t.get("state"))) for t in tasks[:spare - 2]]
        for line in card("REPOSITORY TASKS", body, width, len(body) + 2, unicode):
            add(line, "dim")
    lines += [clipped(line, width) for line in footer]
    visible = [m for lane in lanes for m in teams[lane["participant"]]] if lanes else shown_judges + shown_workers
    items = [("chat", m["participant"]) for m in mains] + [action(m) for m in visible]
    items += [action(m) for m in children] + [action(m) for calls in everyone.values() for m in calls]
    navigation.update(hits=hits, items=list(dict.fromkeys(items)), offset=offset, viewport=3,
                      body_rows=len(children), positions={item: i for i, item in enumerate(items)},
                      room_id=room.get("id"), graph_selected=selected, surface="graph",
                      primary=("chat", primary["participant"]) if primary else None,
                      mains=[m["participant"] for m in mains], bands=bands_hit, band_offsets=scrolled_now)
    text = "\n".join(lines)
    # Plain-word separators keep one-cell ASCII stand-ins, so hit columns do not move.
    return text if unicode else text.translate({ord("·"): "/", ord("→"): ">", ord("×"): "x", ord("│"): "|"})


def action(member):
    return member.get("_action") or ("chat" if member.get("role") == "main" else "result", member["participant"])
