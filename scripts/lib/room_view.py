"""Room graph drawn from participation, addressed messages and lifecycle evidence."""

import calendar
import re
import time

from dashboard_projection import ELLIPSIS, clean, count as valid_count, display_width, fit, listing, mapping
from panel_view import (ATTENTION_STATES, MODEL_NAMES, LIVE_STATES, TERMINAL_STATES, PALETTE, activity, box_edge, box_row,
                        clipped, location_label, main_occupied, menu_rows, status_alerts, worker_rows, wrapped)


def nodes(report):
    room = mapping(report.get("room"))
    attempts = mapping(report.get("attempts"))
    linked = {}
    rows = listing(attempts.get("active_recent")) + listing(attempts.get("recent"))
    # Attempts started before a rollover name the predecessor room; they still belong to this board.
    here = {room.get("id"), room.get("predecessor")} - {None}
    rows = sorted(rows, key=lambda row: mapping(row.get("panel")).get("room_id") not in here)
    for row in rows:
        meta = mapping(row.get("panel"))
        if meta.get("room_id") and meta["room_id"] not in here:
            continue
        ident = meta.get("room_participant")
        if ident and not meta.get("room_id") and ident != row.get("attempt_id"):
            continue
        if meta.get("role") == "main" and not ident:
            ident = row.get("attempt_id")
        if ident:
            linked.setdefault(ident, []).append(row)
    output = []
    finalized = mapping(report.get("finalized"))
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
        state = attempt.get("state") or derived
        # Display only: a review whose task its main completed or accepted reads "decided"; the state stays.
        decided = state == "review" and finalized.get(attempt.get("task_id")) in {"completed", "accepted"}
        output.append(dict(member, attempt=attempt.get("attempt_id"), state=state, _decided=decided,
                           panel_activity=mapping(mapping(report.get("main_activity")).get(member.get("participant"))),
                           ended=result.get("ts"), exit=code if type(code) is int else None,
                           guard=member.get("participant") in guarded,
                           model=model, model_at_launch=meta.get("model") or member.get("model"),
                           task_id=attempt.get("task_id"), location=meta.get("location"),
                           effort=effort, access=meta.get("access"),
                           harness=mapping(attempt.get("panel")).get("harness"),
                           title=member.get("label_override") or meta.get("label") or member.get("label")))
    # A continued worker shares its task with the earlier rounds of the same main.
    rounds = {}
    for member in sorted(output, key=lambda m: m.get("seq", 0)):
        if member.get("role") == "worker" and member.get("task_id") and member.get("parent"):
            earlier = rounds.setdefault((member["parent"], member["task_id"]), [])
            member.update(round=len(earlier) + 1, continues=earlier[-1] if earlier else None)
            earlier.append(member["participant"])
    # A later call of the same role on the same task supersedes an earlier failure; it is no longer attention.
    latest = {}
    for member in sorted(output, key=lambda m: m.get("seq", 0)):
        if member.get("role") != "main" and member.get("task_id"):
            key = (member.get("role"), member["task_id"])
            if key in latest:
                latest[key]["superseded"] = True
            latest[key] = member
    rank = lambda member: (0 if member["state"] in LIVE_STATES else 2 if member["state"] in {"done", "cancelled"} else 1, -member.get("seq", 0))
    return sorted(output, key=rank)


def main_state(member):
    state = member.get("state")
    if member.get("role") != "main":
        return state
    if state == "done":
        return "exited"
    if state not in LIVE_STATES:
        return state
    reading = mapping(member.get("panel_activity"))
    if reading.get("attempt") == member.get("attempt"):
        if reading.get("state") == "waiting":
            if reading.get("kind") == "approval":
                return "waiting_approval"
            if reading.get("kind") == "input":
                return "waiting_input"
        if reading.get("state") == "busy":
            return "working"
        if reading.get("state") == "idle":
            return "idle"
        if reading.get("state") == "quiet":
            return "quiet"
    return "live marker"


def main_words(member, unicode=True):
    state = main_state(member)
    if member.get("role") != "main":
        return said(state)
    if state in {"idle", "quiet", "working", "waiting_approval", "waiting_input"}:
        since = mapping(member.get("panel_activity")).get("since")
        minutes = int(max(0, time.time() - since) // 60) if type(since) in (int, float) and 0 <= since <= time.time() else 0
        word = {"waiting_approval": "needs approval", "waiting_input": "needs input"}.get(state, state)
        return word + ((" · " if unicode else " / ") + "%sm" % minutes if minutes else "")
    return {"working": "working", "live marker": "live"}.get(state, said(state))


def spawn_bar(width, navigation):
    """The board's fixed last row as (text, [(x1, x2, provider)]); None where the board has no live input."""
    installed = navigation.get("installed")
    if installed is None or navigation.get("main_occupied"):
        return None
    if not navigation.get("spawn_managed"):
        return "Start a main: needs the tmux panel (oms panel)", []
    if navigation.get("spawn_armed"):
        keys = [("claude", "1 Claude"), ("pi", "2 Pi"), ("codex", "0 Codex")]
        return "Start one main: " + "  ".join(label for name, label in keys if name in installed) + "  Esc cancel", []
    if navigation.get("spawn_busy"):
        return "Starting a main...", []
    if not installed:
        return "No Claude, Pi or Codex main harness is installed", []
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


def model_name(member):
    name = MODEL_NAMES.get(member.get("model"), member.get("model")) or member.get("provider", "unknown")
    return "Pi / " + name if member.get("harness") == "pi" else name


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


def main_names(members):
    """Display name per main: the model; an earlier main of the room on the same model adds its id tail.

    members come live first, then newest first (nodes), so the live main keeps the plain name."""
    names, seen = {}, set()
    for m in members:
        if m.get("role") == "main":
            name = model_name(m)
            names[m["participant"]] = "%s #%s" % (name, m["participant"][-4:]) if name in seen else name
            seen.add(name)
    return names


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


def closed_tasks(report):
    """Task ids the plan records done or cancelled, or whose owner completed or accepted them."""
    plan = {t.get("id") for t in map(mapping, listing(mapping(report.get("plan")).get("tasks")))
            if t.get("state") in {"done", "cancelled"}}
    return {ident for ident in plan if ident} | {ident for ident, outcome in mapping(report.get("finalized")).items()
                                                  if outcome in {"completed", "accepted"}}


def failure_handled(report, call, read_results=None, closed=None):
    """A consumed result handoff, a later round of its task, or the task's close settles a failure."""
    read_results = handled_results(mapping(report.get("room"))) if read_results is None else read_results
    closed = closed_tasks(report) if closed is None else closed
    return (call["state"] in ATTENTION_STATES - {"waiting_input", "waiting_approval", "review", "blocked"}
            and ("result-" + str(call.get("participant")) in read_results or call.get("superseded") is True
                 or bool(call.get("task_id")) and call.get("task_id") in closed))


def settler(report):
    """(settled, aged) for calls: finished, aged-out or handled calls leave the board; shared with the tree."""
    room = mapping(report.get("room"))
    read_results = handled_results(room)
    finalized = mapping(report.get("finalized"))
    closed = closed_tasks(report)
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
        decided = finalized.get(call.get("task_id")) in {"completed", "accepted"}
        return (call["state"] in FINISHED or call["state"] == "presence unknown" and aged(call)
                or failure_handled(report, call, read_results, closed) or call["state"] in {"review", "blocked"} and decided)
    return settled, aged


def call_classifier(report):
    """Display buckets only; admission and lifecycle evidence retain their own authority."""
    settled, _ = settler(report)
    awaiting = set(listing(report.get("awaiting_admission")))
    # Only a call with a main to sit above can linger there.
    mains = {key for m in nodes(report) if m.get("role") == "main" for key in (m["participant"], m.get("attempt")) if key}

    def classify(call):
        if call["state"] == "review" and not settled(call) or (call["state"] == "done" and call.get("participant") in awaiting):
            return "review"
        return "past" if settled(call) and not (call.get("parent") in mains and lingering(call)) else "live"
    return classify


# Settled ("past") calls leave the board for the dock's Past tab; only calls awaiting their main's decision
# (lifecycle review, or finished and awaiting admission) stay a board group. Plan tasks keep the word "review".
CALL_GROUPS = (("review", "Awaiting decision"),)


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


def elapsed(ts):
    """Compact elapsed UTC time, empty when no valid timestamp was recorded."""
    try:
        seconds = max(0, time.time() - calendar.timegm(time.strptime(str(ts), "%Y-%m-%dT%H:%M:%SZ")))
    except ValueError:
        return ""
    return "<1m" if seconds < 60 else "%dm" % (seconds // 60) if seconds < 3600 else \
        "%dh" % (seconds // 3600) if seconds < 86400 else "%dd" % (seconds // 86400)


def end_words(call, hint):
    if call.get("_decided"):
        return "decided"
    if call.get("state") == "failed":
        if call.get("guard"):
            return "guard stop"
        if call.get("exit") == 124:
            return "timed out"
        if call.get("exit") is not None:
            return "failed (exit %s)" % call["exit"]
    return hint


def call_span(call, arrow="->"):
    """Recorded start/end clocks and consistent state words; live calls include elapsed time."""
    state = call["state"]
    if call.get("_native"):
        return "answered " + call["_finished"] if call.get("_finished") else "asked %s %s running" % (call["_native"], arrow)
    words = end_words(call, said(state))
    if state in LIVE_STATES:
        return " ".join(part for part in (words, elapsed(call.get("joined_at"))) if part)
    end = clock_stamp(call["ended"]) if call.get("ended") else ""
    start = clock_stamp(call.get("joined_at"))
    if state == "presence unknown":
        return (start + " " + arrow + " " if start else "") + words
    if not start:
        return (words + " " + end).strip()
    return "%s %s %s%s" % (start, arrow, end + " " if end else "", words)


JUDGE_LINGER = 1800  # panel_chats.ADVISOR_MAX_AGE: the built-in advisor's answer window


def lingering(call):
    """An answered advisor, reviewer or debate stays above its main for half an hour before the Past tab."""
    if call.get("role") not in {"advisor", "reviewer", "council"} or call["state"] != "done":
        return False
    if str(call.get("participant", "")).startswith("native-advisor-"):
        return True  # panel_chats already drops answers older than the window
    try:
        return time.time() - calendar.timegm(time.strptime(str(call.get("ended")), "%Y-%m-%dT%H:%M:%SZ")) <= JUDGE_LINGER
    except ValueError:
        return False


def joined_seconds(call):
    try:
        return time.time() - calendar.timegm(time.strptime(str(call.get("joined_at")), "%Y-%m-%dT%H:%M:%SZ"))
    except ValueError:
        return None


def live_readers(report, members, settled):
    """Participants that can still read mail, shared by message rows and unread totals.

    Calls stay joined after they end, so a broadcast to "all" stays pending for them forever; only a call that can
    still read its mail counts."""
    mains = [m for m in members if m.get("role") == "main"]
    # A call in review or blocked has ended its process; broadcasts that keep arriving for it are never read.
    reading = LIVE_STATES | {"waiting_input", "waiting_approval"}
    return {m["participant"] for m in mains} | {m["participant"] for m in members if m.get("role") != "main"
                                               and m.get("state") in reading and not settled(m)}


def message_successors(rows, members):
    """Display-only succession uses the continuation validator, never room consumption."""
    from terminal_panel import lineage_ids_from_rows
    mains = [m for m in members if m.get("role") == "main"]
    histories = [(m, lineage_ids_from_rows(rows, m.get("provider"), m["participant"], m.get("attempt")))
                 for m in mains if m.get("attempt") and m.get("state") not in TERMINAL_STATES]
    output = {}
    for old in mains:
        if old.get("state") not in TERMINAL_STATES or not old.get("attempt"):
            continue
        successors = [m["participant"] for m, parents in histories if old["attempt"] in parents]
        if len(successors) == 1:
            output[old["participant"]] = successors[0]
    return output


def message_pending(message, successors):
    return {successors.get(ident, ident) for ident in listing(mapping(message).get("pending_for"))}


def message_address(recipient, members, successors, name_of):
    """Keep the original main visible when mail moves, or when it has nobody to read it."""
    target = successors.get(recipient, recipient)
    label = name_of(target)
    if target != recipient:
        tail = "#" + str(target)[-4:]
        if tail not in label:
            label += " " + tail
        label += " (was #" + str(recipient)[-4:] + ")"
    elif any(m.get("participant") == recipient and m.get("role") == "main"
             and m.get("state") in TERMINAL_STATES for m in members):
        label += " (main exited)"
    return clean(label, 200)


def live_unread(report, members, settled):
    """Unread messages for live readers; a capped projection renders its lower bound as N+."""
    ids = live_readers(report, members, settled)
    room = mapping(report.get("room"))
    successors = mapping(report.get("message_successors"))
    projected = "pending_messages" in room
    count = sum(bool(ids.intersection(message_pending(message, successors)))
                for message in listing(room.get("pending_messages" if projected else "messages")))
    return "%s+" % count if projected and room.get("pending_messages_truncated") else count


def plan_idle(report):
    return valid_count(mapping(report.get("plan")).get("idle_days"))


def held_split(report, members=None):
    """(awaiting, ended) ids of running plan tasks whose current holder's calls all stopped.

    Only the holder's own calls count: the claimant itself, or calls of the claiming main that joined at or after
    the claim, so an earlier holder's calls never speak for a fresh claim. Every counted call terminal: ended.
    Otherwise each open call waits in review, blocked or admission: awaiting.
    A task with no such call, a live or an unknown call stays claimed; a claim never proves a process."""
    admission = set(listing(report.get("awaiting_admission")))
    members = nodes(report) if members is None else members
    owners = {key: m["participant"] for m in members if m.get("role") == "main"
              for key in (m["participant"], m.get("attempt")) if key}
    calls = {}
    for m in members:
        if m.get("role") != "main" and m.get("task_id"):
            calls.setdefault(m["task_id"], []).append(m)
    awaiting, ended = set(), set()
    for task in map(mapping, listing(mapping(report.get("plan")).get("tasks"))):
        if task.get("state") != "running":
            continue
        claimant = task.get("claimed_by") or task.get("claimed_by_participant")
        since = str(task.get("claimed_at") or "")
        states = ["review" if m.get("state") == "done" and m.get("participant") in admission else m.get("state")
                  for m in calls.get(task.get("id"), [])
                  if claimant and m.get("participant") == claimant or
                  (not claimant or owners.get(m.get("parent"), m.get("parent")) == owners.get(claimant, claimant))
                  and not (since and m.get("joined_at") and str(m["joined_at"]) < since)]
        if not states:
            continue
        if all(state in TERMINAL_STATES for state in states):
            ended.add(task["id"])
        elif all(state in TERMINAL_STATES | {"review", "blocked"} for state in states):
            awaiting.add(task["id"])
    return awaiting, ended


def plan_summary(report):
    """(goal, verified, total, review, claimed, stale) of the active plan; None when there is no goal or it idled a week.

    Cancelled tasks are closed, never verified, so they leave the total; stale holds have no live call."""
    plan = mapping(report.get("plan"))
    goal = clean(plan.get("goal"), 200)
    if not plan.get("present") or not goal or (plan_idle(report) or 0) >= 7:
        return None
    by_state = mapping(plan.get("by_state"))
    number = lambda *names: sum(valid_count(by_state.get(name)) or 0 for name in names)
    total = valid_count(plan.get("task_count"))
    total = total if total is not None else sum(valid_count(v) or 0 for v in by_state.values())
    awaiting, ended = held_split(report)
    stale_claims, stale_reviews = (valid_count(plan.get(key)) or 0 for key in ("stale_claims", "stale_reviews"))
    return (goal, number("done"), max(0, total - number("cancelled")),
            max(0, number("review", "landing") - stale_reviews),
            max(0, number("claimed", "running") - stale_claims - len(ended)),
            stale_claims + stale_reviews + len(ended))


def goal_banner(report, width, unicode, compact=False):
    """The one-row board goal, linked by its caller to the full Plan tab."""
    rows = goal_box(report, width, unicode, True)
    land = mapping(report.get("land"))
    if land.get("active") is True:
        sep = " · " if unicode else " / "
        minutes = valid_count(land.get("minutes"))
        text = "LANDING " + clean(str(land.get("sha") or ""))[:7] + sep + clean(str(land.get("step") or "land"))[:12]
        landing = text + (sep + "%sm" % minutes if minutes is not None else "")
        goal = rows[0][0][2:-2].rstrip()
        rows = [(one_row(clipped(goal, max(1, width - display_width(landing) - 7)) + sep + landing, width), "head")]
    return rows


def one_row(text, width):
    """A box short enough for one row: square brackets stand in for the card's edges."""
    text = clipped(text, max(1, width - 4))
    return "[ " + text + " " * max(0, width - display_width(text) - 4) + " ]"


def goal_box(report, width, unicode, compact):
    summary = plan_summary(report)
    sep = " · " if unicode else " / "
    if summary is None:
        return [(one_row("GOAL" + sep + "none" + sep + "oms agent-plan init --goal TEXT", width), "dim")]
    goal, done, total, review, claimed, stale = summary
    filled = min(10, 10 * done // total) if total else 0
    bar = ("▕" + "█" * filled + "░" * (10 - filled) + "▏") if unicode else "[" + "#" * filled + "." * (10 - filled) + "]"
    counts = "%s/%s verified" % (done, total)
    locally_done = sum(1 for task in listing(mapping(report.get("room")).get("repo_tasks"))
                      if task.get("state") != "done" and task.get("id") in mapping(report.get("completed_tasks")))
    if locally_done:
        counts += sep + "%s locally completed" % locally_done
    # A claim is not proof of running work, and work waiting for review is progress: review outlives the bar.
    reviewing, claiming, staling = (sep + "%s %s" % (n, word) if n else "" for n, word in
                                    ((review, "in review"), (claimed, "claimed"), (stale, "stale")))
    options = [bar + " " + counts + reviewing + claiming + staling, bar + " " + counts + reviewing + claiming,
               bar + " " + counts + reviewing, counts + reviewing, "%s/%s%s" % (done, total, reviewing), "%s/%s" % (done, total)]
    inner = max(1, width - 4)
    if compact:
        prefix = "GOAL  "
        right = next((o for o in options if inner - display_width(o) - 2 - len(prefix) >= 12), options[-1])
        text = fit(prefix + goal, max(1, inner - display_width(right) - 2))
        return [(one_row(text + " " * max(2, inner - display_width(text) - display_width(right)) + right, width), "head")]
    top = box_edge("GOAL", width, unicode)
    left, bar_char, closing = ("╭─ GOAL ", "─", "─╮") if unicode else ("+- GOAL ", "-", "-+")
    right = next((o for o in options if width - len(left) - display_width(o) - 6 >= 2), None)
    if right:
        top = left + bar_char * (width - len(left) - display_width(right) - 2 - len(closing)) + " " + right + " " + closing
    note, command = "%s stale, not counted: no live worker" % stale, "oms agent-plan release --id ID"
    # Narrow boxes wrap the note and the release command apart, so the command is never clipped.
    note = [] if not stale else [note + sep + "release with " + command] \
        if display_width(note + sep + "release with " + command) <= inner else \
        wrapped(note, inner, 2) + wrapped(next((text for text in ("release: " + command, command)
                                               if display_width(text) <= inner), command), inner, 2)
    return [(top, "head")] + [(box_row(line, width, unicode), "head") for line in wrapped(goal, inner, 2)] \
        + [(box_row(line, width, unicode), "dim") for line in note] + [(box_edge("", width, unicode, "bottom"), "head")]


def claims_by_main(report):
    """Current claims only, newest first; claiming never proves a live process."""
    held = {}
    for task in listing(mapping(report.get("plan")).get("tasks")):
        task = mapping(task)
        claimant = task.get("claimed_by") or task.get("claimed_by_participant")
        if claimant and task.get("state") in {"claimed", "running", "review"}:
            held.setdefault(claimant, []).append(task)
    for tasks in held.values():
        tasks.sort(key=lambda task: str(task.get("claimed_at") or ""), reverse=True)
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
    state = main_state(member)
    return title + ["%s %s / %s" % (activity(state, frame, unicode), name, main_words(member, unicode)),
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


def padded(text, width):
    text = clipped(text, width)
    return text + " " * max(0, width - display_width(text))


FLANK_WIDTH = 140


def layout(width, count, centered=False):
    cap = 58 if width >= FLANK_WIDTH else width - 8
    span = min(cap, width - 8) if centered else min(cap, (width - count - 1) // count)
    return span, (width - (span * count + count - 1)) // 2


WORDS = {"working": "running", "live marker": "running", "verifying": "checking", "done": "finished",
         "timed_out": "timed out", "orphaned": "lost", "waiting_input": "needs input",
         "waiting_approval": "needs approval", "review": "awaiting decision", "presence unknown": "status unknown"}
WEEKDAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
ROLES = {"main": "Main", "advisor": "Advisor", "reviewer": "Reviewer", "council": "Debate", "worker": "Worker", "researcher": "Researcher"}
MACHINE = re.compile(r"^(stop-reason:|model-route:|model-result:|usage detail:|tokens used|served model|cost usd|"
                     r"##\s*(output|exit|verify)\b|---\s*(begin|end) )", re.I)


def who(member, labels=None, task=False):
    """A participant named with its role, so a Sol worker never reads like the Sol main."""
    if not member:
        return "someone"
    name = mapping(labels).get(member.get("participant")) or model_name(member)
    name += " " + ROLES.get(member.get("role"), "Worker").lower()
    title = clean(member.get("title"))
    if task and member.get("role") != "main" and title and title not in ("Native task not recorded", "Task unrecorded"):
        name += " · " + clipped(title, 28)
    return name


def changed_lines(row):
    """The latest retained patch's bounded file summary, ahead of the answer."""
    changes = next((mapping(c.get("changes")) for c in reversed(listing(row.get("calls")))
                    if mapping(c).get("changes")), {})
    if changes.get("issue"):
        return ["Changed files: unavailable (" + clean(changes["issue"]) + ")"]
    total = valid_count(changes.get("count"))
    if total is None:
        return []
    lines = ["Changed files: %s (+%s -%s)" % (total, valid_count(changes.get("added")) or 0,
                                            valid_count(changes.get("removed")) or 0)]
    files = listing(changes.get("files"))[:12]
    lines += ["  %s +%s -%s" % (clean(f[0]), f[1], f[2]) for f in files
              if isinstance(f, (list, tuple)) and len(f) == 3]
    if total > 12:
        lines.append("  and %s more files" % (total - 12))
    return lines


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
    """The main's declared task with its recorded clock and a stale marker after 30 minutes."""
    found = mapping(mapping(room.get("statuses")).get(participant))
    text = without_now(readable(found.get("text"), " "))
    try:
        then = calendar.timegm(time.strptime(str(found.get("ts")), "%Y-%m-%dT%H:%M:%SZ"))
        stamp = time.strftime("%H:%M", time.localtime(then))
        stale = " (stale)" if time.time() - then > 1800 else ""
    except ValueError:
        stamp, stale = "", ""
    lines = wrapped("Now: " + text + (" (" + stamp + ")" if stamp else "") + stale, width, 2) if text else []
    if lines and stale and width >= len(stale) and not any(stale in line for line in lines):
        lines[-1] = clipped(lines[-1], width - len(stale)) + stale
    return lines


def team_counts(calls, classify=None):
    counts = dict.fromkeys(("attention", "active", "awaiting decision", "finished", "unrecorded"), 0)
    for call in calls:
        state = call.get("state")
        if classify and classify(call) == "review":
            bucket = "awaiting decision"  # the board's group of the same name counts the same calls
        elif state in ATTENTION_STATES and classify and classify(call) == "past":
            bucket = "finished"  # handled, superseded by a later round, or its task closed
        elif state in ATTENTION_STATES:
            bucket = "attention"
        elif state in LIVE_STATES:
            bucket = "active"
        elif state in FINISHED:
            bucket = "finished"
        else:
            bucket = "unrecorded"
        counts[bucket] += 1
    return ["%s %s" % (number, bucket) for bucket, number in counts.items() if number]


UNRECORDED = "no live process, heartbeat or exit seen; not a failure, kept until its main decides"


def detail(member, preview, width, members=(), room=None, teams=None, everyone=None, labels=None, claims=None, classify=None):
    """Selected block content in plain words; reading mail here never acknowledges it."""
    room = room or {}
    names = {m["participant"]: who(m, labels) for m in members}
    name = mapping(labels).get(member.get("participant")) or model_name(member)
    state = main_state(member)
    title = clean(member.get("title")) or ""
    title = "" if title in ("Native task not recorded", "Task unrecorded") else title
    where = "in the repository" if member.get("location") in (None, "repository") else "in its own worktree"
    targets = {}
    if classify is None:
        awaiting = set(listing(room.get("awaiting_admission")))
        classify = lambda c: "review" if c.get("participant") in awaiting else "live"
    if member.get("role") == "main":
        lines = wrapped("%s · %s main · %s" % (name, (member.get("provider") or "").capitalize(), main_words(member)), width, 2)
        held = (claims if claims is not None else claims_by_main({"plan": {"tasks": room.get("repo_tasks")}})).get(member["participant"], [])
        if not title and held:
            title = clean(held[0].get("title")) or clean(held[0].get("id"))
        lines += wrapped("Task: " + (title or "no task title yet"), width, 2)
        lines += declared_status(room, member["participant"], width)
        if member.get("owns"):
            lines += wrapped("Scope: " + ", ".join(clean(scope) for scope in listing(member["owns"])), width, 2)
        if not member.get("model") and member.get("model_at_launch"):
            lines += wrapped("Current model unavailable · launch setting: " +
                             MODEL_NAMES.get(member["model_at_launch"], member["model_at_launch"]), width, 2)
        from panel_messages import main_mail
        inbox = main_mail(room, member["participant"])
        unread = sum(valid_count(mapping(p).get("pending")) or 0 for p in listing(room.get("pairs"))
                     if mapping(p).get("recipient") == member["participant"])
        calls = sorted((everyone or teams or {}).get(member["participant"], []), key=call_order)
        deciding = [c for c in calls if classify(c) == "review"]
        attention = [c for c in calls if c["state"] in ATTENTION_STATES and classify(c) not in {"past", "review"}]
        active = [c for c in calls if c["state"] in LIVE_STATES and c not in attention + deciding]
        finished = sorted((c for c in calls if c not in attention + deciding and (
                              c["state"] in FINISHED or c["state"] in ATTENTION_STATES and classify(c) == "past")),
                          key=lambda c: (str(c.get("ended") or c.get("joined_at") or ""), c.get("seq", 0)), reverse=True)
        unknown = [c for c in calls if c not in attention + deciding + active + finished]
        lines.append("Team: " + (" · ".join(team_counts(calls, classify)) or "no calls yet"))
        native = next((c for c in calls if c.get("_native")), None)
        if native:
            # A summary of the main's advisor state belongs beside the team counts, not below the call log.
            lines.append("Built-in advisor: " + ("asked %s · running" % native["_native"] if native["state"] != "done"
                                                 else "last answered %s" % (native.get("_finished") or native["_native"])))

        def team_rows(heading, group):
            if group:
                lines.append("  " + heading)
            for c in group:
                label = MODEL_NAMES.get(c.get("model"), c.get("model")) or "unknown"
                span = call_span(c, "→") + (", awaiting decision" if c in deciding and c["state"] == "done" else "")
                text = "- %s%s %s · %s · %s" % ("! " if c in attention + deciding else "", ROLES.get(c.get("role"), "Worker"),
                        label, span, clean(c.get("title")) or "no task title")
                parts = wrapped(text, max(1, width - 4), max(1, len(text)))
                for index, part in enumerate(parts):
                    targets[len(lines)] = action(c)
                    lines.append(("  " if index == 0 else "    ") + part)
        team_rows("Needs attention", attention)
        team_rows("Awaiting decision", deciding)
        team_rows("Active", active)
        team_rows("Unrecorded", unknown)
        if unknown:
            lines.extend("    " + part for part in wrapped(UNRECORDED, max(1, width - 4), len(UNRECORDED)))
        team_rows("Finished", finished[:5])
        if len(finished) > 5:
            lines.append("%s earlier finished · see the Past tab" % (len(finished) - 5))
        lines.append("Messages to %s: %s%s" % (who(member, labels), len(inbox), " (%s unread)" % unread if unread else ""))
        for m in inbox[-3:]:
            sender = names.get(m.get("sender"), clean(m.get("sender")))
            text = readable(m.get("text"), "\n", True) or "(empty)"
            preview = []
            truncated = False
            for source_index, source_line in enumerate(text.splitlines()):
                if not source_line.strip():
                    continue
                prefix = "From %s: " % sender if not preview else ""
                available = max(1, width - 4 - display_width(prefix))
                parts = wrapped(source_line, available, max(1, len(source_line))) if source_line else [""]
                for part in parts:
                    if len(preview) == 2:
                        truncated = True
                        break
                    preview.append(prefix + part)
                    prefix = ""
                if truncated:
                    break
            if truncated and preview:
                preview[-1] = clipped(preview[-1], max(0, width - 7)) + "..."
            for index, part in enumerate(preview[:2]):
                lines.append(("  " if index == 0 else "    ") + clipped(part, max(0, width - (2 if index == 0 else 4))))
        if not inbox:
            lines.append("  No messages received")
        lines += ["", "Works %s%s" % (where, " · effort " + member["effort"] if member.get("effort") else "")]
        return lines + ["Click a team row to open it · click the main for its chat"], targets
    if member.get("_native"):
        done = member["state"] == "done"
        return ["%s advisor · %s" % (member.get("model"), "answered" if done else "running"),
                ("Answered %s" % (member.get("_finished") or member["_native"]) if done else "Started %s" % member["_native"])
                + " · Claude Code's built-in advisor",
                "Its answer stays inside the main's chat"], targets
    owner = names.get(member.get("parent"), "")
    lines = wrapped("%s%s · %s%s" % (who(member), " for " + owner if owner else "",
                                     end_words(member, said(state)), " · " + title if title else ""), width, 3)
    if member.get("round", 1) > 1:
        lines += wrapped("Round %s of this task, continuing %s" % (member["round"], names.get(member.get("continues"), "its earlier worker")), width, 2)
    if member.get("exit") == 124:
        lines += wrapped("Timed out · its main can continue it", width, 3)
    if state == "presence unknown":
        note = "Status unknown: " + UNRECORDED + ". Any recorded result is below; f opens it"
        lines += wrapped(note, width, len(note))
    access = {"write": "can edit", "read": "read-only"}.get(member.get("access"), "permissions unrecorded")
    metadata = "%s %s%s" % (access.capitalize(), where, " · effort " + member["effort"] if member.get("effort") else "")
    loaded = preview.get("report")
    if preview.get("error"):
        return lines + ["", readable(preview["error"], " "), "", metadata], targets
    if loaded is None:
        return lines + ["", "Loading its recorded result...", "", metadata], targets
    if not listing(loaded.get("rows")):
        return lines + ["", "No result recorded yet (%s)" % end_words(member, said(state)), "", metadata], targets
    # A whole-main result is adopted by every child of that main; only the call's own task speaks for it.
    rows = listing(loaded.get("rows"))
    row = next((r for r in rows if member.get("task_id") and r.get("task_id") == member["task_id"]), rows[0])
    answer = readable(latest_answer({"rows": [row]}), paragraphs=True)
    outcome = {"accepted": "Accepted by its main", "completed": "Completed; the main recorded a summary",
               "failed": "Marked failed by its main"}.get(row.get("outcome"), "Not reviewed by its main yet")
    check = mapping(row.get("verification")).get("status")
    lines += ["", outcome + " · " + ("checks %s" % str(check).replace("_", " ") if check else "no checks run")]
    # The changed files are a bounded summary of the outcome: they come before long prose, which pages below.
    changes = next((c["changes"] for c in reversed(listing(row.get("calls"))) if c.get("changes")), None)
    if changes and changes.get("issue"):
        lines += ["", "Changed files: unavailable (" + changes["issue"] + ")"]
    elif changes and changes.get("count"):
        lines += ["", "Changed files: %s (+%s -%s)" % (changes["count"], changes["added"], changes["removed"])]
        lines += ["  %s +%s -%s" % tuple(f) for f in changes["files"][:12]]
        lines += ["  and %s more files" % (changes["count"] - 12)] if changes["count"] > 12 else []
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
            for index, part in enumerate(wrapped(paragraph, max(1, width - 4), limit + 1, max_chars=4000)):
                if len(body) == limit:
                    more = True
                    break
                body.append(("  " if index == 0 else "    ") + part)
            more = more or cut
            if more:
                break
        lines.extend(["", heading] + body)
        if more:
            lines.append("  More in the full recorded result · Enter opens it")

    if summary:
        result_text("Main's summary:", summary, 24)
    if answer and answer != summary:
        result_text("Worker's answer:" if summary else "Answer:", answer, 12 if summary else 40)
    lines += ["", metadata]
    if str(member.get("location") or "").startswith("worktree:") and member.get("state") in FINISHED | {"failed"}:
        lines.append("w opens its worktree in a shell")
    return lines, targets


FINISHED = {"done", "cancelled"}


def board_summary(report, calls, classify, width, rows, unicode=True, ranked=None):
    """Attention, In progress and Recent in at most `rows` rows; the board's own counts, in the same words."""
    from panel_view import attention_items, attention_row
    sep = " · " if unicode else " / "
    calls = list({action(c): c for c in calls if not c.get("_native")}.values())
    ranked = attention_items(report, unicode) if ranked is None else ranked
    attention = [attention_row(item, 400, unicode) for item in ranked]
    running = [c for c in calls if c["state"] in LIVE_STATES]
    oldest = min((str(c["joined_at"]) for c in running if c.get("joined_at")), default="")
    recent = sorted((c for c in calls if c["state"] in TERMINAL_STATES and c.get("ended")),
                    key=lambda c: str(c["ended"]), reverse=True)
    recent = [sep.join(p for p in ("%s %s" % (model_name(c), ROLES.get(c.get("role"), "Worker").lower()),
                                   end_words(c, said(c["state"])) + " " + clock_stamp(c["ended"]) +
                                   (", awaiting decision" if c["state"] == "done" and classify(c) == "review" else ""),
                                   clean(c.get("title"))) if p) for c in recent]
    sections = [["Attention    %s" % len(attention) + (sep + attention[0] if attention else sep + "nothing waiting")]
                + attention[1:2],
                ["In progress  %s call%s" % (len(running), "" if len(running) == 1 else "s") +
                 (sep + "oldest " + elapsed(oldest) if oldest else "")],
                ["Recent       " + (recent[0] if recent else "no finished calls yet")] + recent[1:3]]
    # Each section keeps its first row; spare rows go to attention, then recent.
    keep = [1, 1, 1]
    for index in (0, 2, 2):
        if sum(keep) < rows and keep[index] < len(sections[index]):
            keep[index] += 1
    return [clipped((" " * 13 if n else "") + text, width)
            for section, count in zip(sections, keep) for n, text in enumerate(section[:count])][:rows]


def latest_answer(report):
    for row in listing(mapping(report).get("rows")):
        for call in reversed(listing(row.get("calls"))):
            if call.get("answer"):
                return call["answer"]
        if row.get("summary"):
            return row["summary"]
    return None


def render_graph(report, width, height, color=False, unicode=True, frame=None, menu=False,
                 attention_only=False, main_attempt=None, navigation=None, managed=False,
                 previous=None):
    """Page real parent relationships without inventing routers or progress."""
    from panel_view import tone
    from dashboard_projection import use_unicode
    use_unicode(unicode)
    interactive = navigation is not None
    navigation = navigation if interactive else {}
    navigation.pop("box", None)
    navigation.update(tab_strip_visible=True, tab_body_visible=True, help_rows=max(4, height // 3))
    room = mapping(report.get("room"))
    members = nodes(report)
    navigation["main_occupied"] = bool(
        navigation.get("room_poll_unknown") or navigation.get("capacity_stale") or report.get("capacity_stale")
        or room.get("error") or mapping(report.get("collection")).get("ok") is not True
        or main_occupied(report, members))
    members += native_advisors(report, members)
    labels = main_names(members)
    # Earlier mains of the room stay readable; the live one leads.
    mains = [m for m in members if m.get("role") == "main"]
    selected = navigation.get("selected")
    owners = {key: m for m in mains for key in (m["participant"], m.get("attempt")) if key}
    primary = owners.get(main_attempt) or next((m for m in mains if m["state"] in LIVE_STATES), next(iter(mains), None))
    picked = next((m for m in members if selected and selected[1] == m["participant"]), None)
    if picked:
        primary = picked if picked.get("role") == "main" else owners.get(picked.get("parent"), primary)
    debates = [row for row in worker_rows(report) if row["source"] == "council"]
    classify = call_classifier(report)
    # A sole main, or the one live main, needs no "for <main>" on its calls; an exited predecessor's calls keep it.
    # A main with no attempt evidence ("presence unknown") is not shown to be live, so it never needs disambiguation.
    sole_live = bool(primary) and (len(mains) == 1 or [m for m in mains if m["state"] not in
                                                       TERMINAL_STATES | {"presence unknown"}] == [primary])

    def team(main, every=False):
        ids = {key for key in (main["participant"], main.get("attempt")) if key}
        calls = [{"participant": row["task"], "role": "council", "model": row["name"],
                  "title": row["work"], "state": row["state"], "parent": row["parent"],
                  "_action": ("debate", (row["task"], main["participant"]))}
                 for row in debates if row["parent"] in ids]
        calls += [m for m in members if m.get("role") != "main" and m.get("parent") in ids]
        if attention_only:
            calls = [m for m in calls if classify(m) != "past" and
                     (m["state"] in ATTENTION_STATES or classify(m) == "review")]
        # Judges precede workers so keyboard order follows the drawing.
        return sorted(calls, key=lambda m: (m.get("role") in {"worker", "researcher"},) + call_order(m))

    teams = {m["participant"]: team(m, every=True) for m in mains}
    grouped = {key: {kind: [c for c in calls if classify(c) == kind] for kind, _ in CALL_GROUPS}
               for key, calls in teams.items()}
    if navigation.get("group_tree"):
        if any(grouped.get(owner, {}).get(kind) for kind, owner in navigation.get("expanded_groups", set())):
            from panel_tree import render_tree
            return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                               main_attempt, navigation, managed=managed, previous=previous)
        navigation.pop("group_tree", None)
    linked_children = team(primary, every=True) if primary else []
    # Finished calls leave the board (results stay in the main's detail and the tree); attention stays.
    settled, _ = settler(report)
    everyone = teams
    teams = {key: [c for c in calls if classify(c) == "live"] for key, calls in teams.items()}
    children = teams[primary["participant"]] if primary else []
    # Running judges first; answered ones linger after them.
    judges = sorted((m for m in children if m.get("role") in {"advisor", "reviewer", "council"}),
                    key=lambda m: m["state"] in FINISHED)
    workers = [m for m in children if m.get("role") in {"worker", "researcher"}]
    unlinked = [m for m in members if m.get("role") != "main" and m.get("parent") not in owners]
    if attention_only:
        unlinked = [m for m in unlinked if classify(m) != "past" and
                    (m["state"] in ATTENTION_STATES or classify(m) == "review")]
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
    preview = mapping(navigation.get("preview"))
    previewed = next((m for m in mains + [c for calls in everyone.values() for c in calls] + unlinked
                      if action(m) == preview.get("target")), None)
    navigation["full_mail"] = None
    navigation.pop("full_result", None)
    if previewed and previewed.get("role") != "main" and not previewed.get("_native"):
        navigation["full_result"] = action(previewed)
    if menu:
        footer = menu_rows(
            width, 1, unicode, navigation.get("menu_help", False), managed,
            occupied=navigation["main_occupied"], installed=navigation.get("installed"))
    else:
        chosen_member = next((m for m in mains + [c for calls in everyone.values() for c in calls]
                              if action(m) == selected), None)
        footer = footer_hints(navigation, width, managed, unicode, chosen_member, True)
    hints = list(footer)
    hint_rows = len(footer)
    if navigation.get("notice"):
        if height >= 20 or menu:
            footer.append(clean(navigation["notice"], 180))
        else:
            # A short board shows the notice in place of the hints, never silently.
            fixed = "  ".join(footer_hints({}, width, managed, unicode, None, True)[-1].split("  ")[-2:])
            footer = [clipped(clean(navigation["notice"], 180), max(1, width - display_width(fixed) - 2)) + "  " + fixed]
            hint_rows = 1
    if menu and previous is not None:
        from panel_view import message_rows
        last_rows = (message_rows("Last: " + previous, width)
                     if "\n" in previous or "oms panel --release-main " in previous else ["Last: " + previous])
        footer = footer[:max(0, height - 1 - len(last_rows))] + last_rows[:max(0, height - 1)]
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

    claims = claims_by_main(report)
    awaiting_tasks, ended_tasks = held_split(report, members)

    def claim_line(m):
        held = claims.get(m["participant"], [])
        if not held:
            return ""
        # A task whose calls all ended is not running work; one whose calls wait in review is counted apart.
        current = [t for t in held if t.get("id") not in awaiting_tasks | ended_tasks]
        lead = (current or held)[0]
        word = "awaiting decision" if lead.get("id") in awaiting_tasks else "worker ended" \
            if lead.get("id") in ended_tasks else "in review" if lead.get("state") == "review" else lead.get("state")
        waiting = sum(t.get("id") in awaiting_tasks and t is not lead for t in held)
        return "Task: %s · %s" % (lead.get("id"), word) + (
            " · +%s more running" % (len(current) - 1) if len(current) > 1 else "") + (
            " · %s awaiting decision" % waiting if waiting else "")

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
        return "Now: " + text + ((" · " if unicode else " / ") + ago(stamp) + " (stale)"
                                 if seconds > 1800 else "") if text else ""

    def task_of(m):
        task = clean(m.get("title")) or "Task unrecorded"
        known = task not in ("Native task not recorded", "Task unrecorded")
        if m.get("role") == "main":
            # The plan task a main holds rides beside its title; without one its declared status stands in.
            return " · ".join(part for part in (task if known else "", claim_line(m), "" if known else now_line(m)) if part) \
                or "no status yet"
        return task if known else "no task title yet"

    def task_lines(m, span, limit):
        title = task_of(m)
        now = now_line(m) if m.get("role") == "main" else ""

        def status_fit(text, available):
            stale = " (stale)" if " (stale)" in text and available > len(" (stale)") else ""
            return clipped(text.replace(stale, "") if stale else text, max(1, available - len(stale))) + stale

        if now and clean(m.get("title")) not in (None, "", "Native task not recorded", "Task unrecorded"):
            if limit == 1:
                return [clipped(title, max(1, span // 2 - 2)) + " / " + status_fit(now, max(1, span // 2 - 1))]
            return wrapped(title, span, 1) + [status_fit(now, span)]
        parts = wrapped(title, span, limit)
        if now and "(stale)" in now and not any("(stale)" in line for line in parts):
            parts[-1] = status_fit(parts[-1] + " (stale)", span)
        return parts

    # Mail to finished calls is never read again; unread mail to mains and running calls is counted.
    unread_total = live_unread(report, members, settled)
    import os
    repo = mapping(report.get("repo"))
    heading_text = "OMS · " + (os.path.basename(clean(repo.get("name")) or str(repo.get("path") or "").rstrip("/\\")) or "repository")
    clock = time.strftime("%m-%d ") + WEEKDAYS[time.localtime().tm_wday] + time.strftime(" %H:%M")
    if display_width(heading_text) + len(clock) + 2 <= width:
        heading_text += " " * (width - display_width(heading_text) - len(clock)) + clock
    add(heading_text, "head")
    from panel_metrics import usage_line
    add(usage_line(report, width, unicode), "dim")
    for text, style in goal_banner(report, width, unicode, compact=height < 20):
        hits.append({"y": len(lines) + 1, "x1": 1, "x2": width, "action": ("tab", "plan")})
        add(text, style)
    for alert in status_alerts(report):
        add(alert, "bad")
    from panel_view import attention_items, needs_strip
    # One ranked queue per frame feeds the strip, the spare-row summary and the Attention tab.
    ranked = attention_items(report, unicode)
    attention_rows = needs_strip(report, width, unicode, ranked=ranked)
    for row in attention_rows:
        if row["action"]:
            hits.append({"y": len(lines) + 1, "x1": 1, "x2": width, "action": row["action"]})
        add(row["text"], row["style"])
    group_actions = []
    if primary:
        group_labels = []
        for kind, title in CALL_GROUPS:
            calls = grouped[primary["participant"]][kind]
            if not calls:
                continue
            target = ("group", (kind, primary["participant"]))
            group_actions.append(target)
            label = "%s %s (%s)" % ("▸" if unicode else ">", title, len(calls))
            if not sole_live:
                label += " / " + name_of(primary)
            group_labels.append((label, target, "review" if kind == "review" else "dim"))
        if sole_live and group_labels:
            group_width = max(1, (width - 3 * (len(group_labels) - 1)) // len(group_labels))
            group_labels = [(clipped(label, group_width), target, style) for label, target, style in group_labels]
            column = 1
            for label, target, unused in group_labels:
                hits.append({"y": len(lines) + 1, "x1": column,
                             "x2": min(width, column + display_width(label) - 1), "action": target})
                column += display_width(label) + 3
            add(" · ".join(label for label, unused, style in group_labels), "dim")
        else:
            for label, target, style in group_labels:
                hits.append({"y": len(lines) + 1, "x1": 1, "x2": width, "action": target})
                add(label, style)
    others = listing(room.get("other_rooms"))
    if room.get("other_rooms_incomplete") and height >= 20 and not others:
        add("OTHER ROOMS / scan incomplete; mains in other rooms may be missing", "alert")
    if others and height >= 20:
        # Other rooms may hold history or mains outside this panel; keep their graphs separate from this single-main board.
        add("OTHER ROOMS" + (" (scan incomplete)" if room.get("other_rooms_incomplete") else "") + " / " + " | ".join("%s %s live main%s%s" % (clean(r.get("id"), 24), r.get("mains"),
            "s" if r.get("mains") != 1 else "",
            " / %s pending" % r["pending"] if r.get("pending") else "") for r in others[:3]) +
            (" | +%s" % (len(others) - 3) if len(others) > 3 else "") + " / o Rooms in control", "alert")
    total_rows = max(0, height - int(menu) - len(footer))
    used = len(lines)
    wire_rows = 2 if height >= 32 else 1
    base_wires = wire_rows * (bool(shown_judges) + bool(shown_workers))
    # Empty advisor/worker places stay visible, one row each, so the structure never changes shape.
    base_wires += (not shown_judges) + (not shown_workers) if primary and height >= 24 else 0

    def full_title(m, reflow=False):
        role = "MAIN" if m.get("role") == "main" else m.get("role", "worker").upper()
        return role + (" " if reflow else " / ") + name_of(m)

    def heading(m):
        mark = ("▸ " if unicode else "> ") if action(m) == selected else ""
        return mark + full_title(m)

    def compact(group, span):
        # The top wire must meet the card midpoint, even with a selection mark.
        limit = span // 2 - 4 if any(m.get("role") in {"main", "worker", "researcher"} for m in group) else span - 6
        return any(display_width(heading(m)) > limit for m in group)

    shown_groups = [g for g in (shown_judges, shown_workers) if g] + ([[primary]] if primary else [])

    def minimum_rows(group):
        span = layout(width, len(group), group == [primary])[0]
        return 4 + max(len(task_lines(m, span - 4, 2)) for m in group) if compact(group, span) else 3

    minimum_total = sum(minimum_rows(g) for g in shown_groups)

    def plan_upper(rows_left):
        """Wire rows and embedded branches of the main-and-bands board in `rows_left` rows."""
        wires, embedded = base_wires, {}
        if primary and height <= 20 and rows_left - wires < minimum_total:
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
        return embedded, wires

    # Reserve the ordinary band shape even for its empty placeholders, so
    # call counts and finished groups cannot move the reading dock.
    placeholders = bool(primary) and height >= 24
    ordinary = (3 if primary else 0) + sum(wire_rows + 6 for group in
                (shown_judges, shown_workers) if group or placeholders)
    worker_floor = 5 if layout(width, 3)[0] // 2 - 4 < 21 else 3
    compact_shape = 3 + 2 * wire_rows + 3 + worker_floor if primary else 0
    fixed_header = 3 + len(status_alerts(report)) + len(attention_rows)
    fixed_header += int(bool(others or room.get("other_rooms_incomplete")) and height >= 20)
    target = max(0, total_rows - fixed_header - ordinary - 1) if interactive else 0
    compact_target = min(height * 2 // 5, max(0, total_rows - fixed_header - compact_shape - 1)) if interactive else 0
    target = max(target, compact_target)
    if height <= 20:
        target = min(height * 2 // 5, height - 17) if interactive else 0
    target = target if target >= 3 else int(interactive)
    warn_possible = int(bool(unlinked) or len(children) > len(shown_judges) + len(shown_workers))

    def upper_fit(rows_left, stable):
        """(embedded branches, wire rows) when the upper region fits rows_left, else None."""
        embedded, wires = plan_upper(rows_left)
        # Group headings use space vacated by finished calls without moving the dock.
        return (embedded, wires) if rows_left >= max(wires + minimum_total,
                                                    (ordinary + fixed_header - used) * stable) else None

    plan = None
    tiers = ((True, max(11, compact_target), target), (False, 3, target), (False, 1, min(target, 1)), (False, 0, 0)) if interactive else ((False, 0, 0),)
    # A visible warning outranks a bigger box, but not the tab strip; the strip outranks the warning.
    for warn in (1, 0) if warn_possible else (0,):
        for stable, low, high in tiers:
            if stable and height <= 20 or high < low or warn and not high:
                continue
            for dock_rows in range(high, low - 1, -1):
                got = upper_fit(total_rows - used - dock_rows - (warn or stable) * bool(dock_rows), stable)
                if got is not None:
                    plan = (dock_rows, got, (warn or stable) * bool(dock_rows))
                    break
            if plan:
                break
        if plan:
            break
    if plan is None:
        navigation["tab_strip_visible"] = False
        from panel_view import render
        navigation.update(hits=[], items=[], viewport=0, positions={})
        navigation.pop("full_result", None)
        text = render(report, primary.get("provider", "codex") if primary else "codex", width, height,
                      color, unicode, menu=menu, managed=managed, main_attempt=main_attempt, frame=frame,
                      view="tree" if interactive else "compact", attention_only=attention_only, navigation=navigation,
                      previous=previous)
        return text
    dock, (embedded, wires), warn_reserve = plan
    budget = total_rows - dock - warn_reserve

    def extras(m):
        return int(bool(m.get("location")))

    def needed(group, centered=False):
        span = layout(width, len(group), centered)[0]
        reflow = int(compact(group, span))
        return max(3 + (1 if m.get("role") == "main" else extras(m)) + reflow + len(task_lines(m, span - 4, 2)) for m in group)

    # Cards never keep blank rows; rows they do not need go to the detail area.
    def drawn_rows(card_height):
        return sum(max(minimum_rows(g), min(card_height, needed(g, g == [primary]))) for g in shown_groups)

    available_cards = budget - used - wires
    card_height = next((candidate for candidate in range(6, 2, -1)
                        if drawn_rows(candidate) <= available_cards), 3)
    automatic = False
    if interactive and not previewed and primary and dock >= 5 and not navigation.get("dismissed"):
        # The box shows the shown main by default; it never displaces a choice.
        previewed, preview, automatic = primary, {"target": action(primary), "report": {}}, True

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
                title = ""
            state = main_state(m)
            words = end_words(m, main_words(m, unicode))
            if m.get("role") != "main" and state in LIVE_STATES | FINISHED:
                words = call_span(m, "→" if unicode else "->")
            status = "%s %s" % (activity(state, frame, unicode), words)
            if m.get("role") == "main" and open_count(room, m["participant"]):
                status += (" · " if unicode else " / ") + "? %s open" % open_count(room, m["participant"])
            ctx = context_note(report, m)
            if ctx:
                status += "".join((" · " if unicode else " / ") + part for part in
                                  ["ctx %s%%" % ctx[0]] + ["compact soon"] * (ctx[1] == "alert"))
            if m.get("role") != "main" and primary and not sole_live:
                status += " · for " + clean(name_of(primary))
            task = task_of(m)
            content = [status + " / " + task] if rows_h == 3 else [status] + task_lines(m, span - 4, rows_h - 3)
            if rows_h >= 4:
                where = ("@ " + location_label(m["location"]) + (" / " + m["effort"] if m.get("effort") else "")) if m.get("location") else ""
                if m.get("role") == "main":
                    where = "Team: " + (" · ".join(team_counts(everyone.get(m["participant"], []), classify)) or "no calls yet")
                tail = [where] if where and rows_h >= 5 + int(reflow) else []
                first = (("▸ " if unicode else "> ") if action(m) == selected else "") + full_title(m, reflow=True)
                content = ([first] if reflow else []) + [status] + task_lines(
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
            flanks = centered and width >= FLANK_WIDTH and rows_h >= 5
            if flanks:
                if row == 0:
                    left_width, right_width = left - 2, width - left - cell_width - 2
                    linked_workers = [m for m in linked_children if m.get("role") in {"worker", "researcher"}]
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
                    cell_style = "dim" if row == 1 and main_state(group[i]) == "idle" else tone(group[i]["state"]) if row == 1 and group[i]["state"] in ATTENTION_STATES else style
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

    scrolled_now = {"judge": judge_start, "worker": worker_start}
    main_width, main_left = layout(width, 1, True)
    root_bottom = main_left + main_width // 2
    root_top = root_bottom
    if shown_judges:
        band(shown_judges, "review", "judge", True)
    elif primary and height >= 24:
        hint = "◇ no advisors · a ask" if unicode else "no advisors / a ask"
        left = min(max(0, root_top - 10), max(0, width - display_width(hint)))
        add(padded(" " * left + hint, width), "dim")
    if primary:
        cards([primary], hue(primary), centered=True,
              top=bool(shown_judges), bottom=bool(shown_workers))
    else:
        prompt = "No main connected · " if navigation["main_occupied"] else "No main · n, then "
        prompt += menu_rows(width, 1, unicode, managed=managed, occupied=navigation["main_occupied"],
                            installed=navigation.get("installed"))[0]
        for line in card("MAIN", [prompt], width, card_height, unicode):
            add(line, "main")
    if shown_workers:
        band(shown_workers, "worker", "worker", False)
    elif primary and height >= 24:
        add(" " * max(0, root_bottom - 10) + ("▢ no workers" if unicode else
                                               "no workers"), "dim")
    body_drawn = False
    omitted = len(children) - len(shown_judges) - len(shown_workers)
    parts, open_unlinked = ["%s more calls (scroll)" % omitted] if omitted else [], []
    if omitted or unlinked:
        open_unlinked = [m for m in unlinked if classify(m) != "past"]
        if open_unlinked:
            # Settled ones are listed in the Past tab; only open calls need the tree.
            parts.append("%s call%s with no known main%s · t shows them" % (
                len(open_unlinked), "" if len(open_unlinked) == 1 else "s",
                " (+%s past)" % (len(unlinked) - len(open_unlinked)) if len(open_unlinked) < len(unlinked) else ""))
    # Rows the cards and the box leave blank carry a display-only summary; it never takes their rows.
    spare = total_rows - dock - len(lines) - bool(parts) - 1
    if dock and spare >= 3:
        lines.append("")
        for text in board_summary(report, [c for calls in everyone.values() for c in calls] + unlinked,
                                  classify, width, spare, unicode, ranked):
            add(text, "dim")
    if omitted or unlinked:
        if parts and len(lines) < total_rows - dock:
            # The warning sits on the row just above the box, so it holds still as the upper region changes.
            lines.extend([""] * max(0, total_rows - dock - 1 - len(lines)) * bool(dock))
            add(" / ".join(parts), "alert" if open_unlinked else "dim")
    # The box keeps the rows it was given whatever the upper region drew; only an overflowing region shrinks it.
    rows_high = min(dock, max(0, total_rows - len(lines)))
    rows_high = 1 if 0 < rows_high < 3 else rows_high
    navigation.update(tab_strip_visible=bool(rows_high), tab_body_visible=rows_high >= 3)
    if rows_high:
        lines.extend([""] * (total_rows - rows_high - len(lines)))
        dock_lines, dock_hits = tab_box(report, width, rows_high, navigation, color, unicode,
            members=members, primary=primary, previewed=previewed, preview=preview, teams=teams,
            everyone=everyone, labels=labels, main_attempt=main_attempt, automatic=automatic,
            claims=claims, classify=classify, attention=ranked,
            past=past_calls([c for calls in everyone.values() for c in calls] + unlinked, classify))
        top = len(lines)
        lines += dock_lines
        hits += [dict(hit, y=hit["y"] + top) for hit in dock_hits]
        body_drawn = navigation["tab_body_visible"]
        scrolled_now["detail"] = navigation.get("band_offsets", {}).get("detail", 0)
        if navigation.get("box"):
            navigation["box"] = {**navigation["box"], "y1": navigation["box"]["y1"] + top,
                                 "y2": navigation["box"]["y2"] + top}
            bands_hit.append(dict(navigation["box"], name="detail", x1=1, x2=width, step=3))
    if body_drawn and not menu and navigation.get("tab") not in navigation.get("visible_tabs", []) + [None, "detail"] \
            and footer[-hint_rows:] == hints:
        # The hints were fitted before the dock fell back to Detail for a tab it does not show.
        fitted_hints = footer_hints(navigation, width, managed, unicode, chosen_member, True)
        footer[-hint_rows:] = fitted_hints[:hint_rows] + [""] * max(0, hint_rows - len(fitted_hints))
    if body_drawn and navigation.get("tab") == "debate" and not navigation.get("debate_view", {}).get("shown") and not menu:
        footer[-hint_rows:] = footer_hints(dict(navigation, debate_empty=True), width, managed, unicode, chosen_member, True)
    if navigation.get("full_mail") and not menu:
        fitted = footer_hints(navigation, width, managed, unicode, chosen_member, True)
        footer[:hint_rows] = fitted[:hint_rows] + [""] * max(0, hint_rows - len(fitted))
    if not body_drawn:
        navigation.pop("full_result", None)
    if not body_drawn and not menu and footer[-hint_rows:] == hints:
        # A strip can switch tabs without a body; an absent strip offers neither action.
        fitted_hints = footer_hints(dict(navigation, preview=None, detail=None, full_result=None, tab_body_visible=False),
                                    width, managed, unicode, chosen_member, True)
        footer[-hint_rows:] = fitted_hints + [""] * max(0, hint_rows - len(fitted_hints))
    lines += [""] * max(0, height - int(menu) - len(lines) - len(footer))
    lines += [clipped(line, width) for line in footer]
    if bar:
        row = len(lines) - (hint_rows + 1 if not menu else 1)
        lines[row] = paint(lines[row], "main" if bar[1] else "dim")
        hits += [{"y": row + 1, "x1": x1, "x2": x2, "action": ("spawn", name)} for x1, x2, name in bar[1]]
    visible = shown_judges + shown_workers
    # Keys reach only what this board draws: its main and that main's calls.
    items = [("chat", primary["participant"])] if primary else []
    items += [action(m) for m in visible]
    items += [action(m) for m in everyone.get(primary["participant"], []) if classify(m) == "live"] if primary else []
    items += group_actions
    items += [hit["action"] for hit in hits if hit["action"][0] in {"result", "debate"}]
    navigation.update(hits=hits, items=list(dict.fromkeys(items)), offset=offset, viewport=3,
                      body_rows=len(children), positions={item: i for i, item in enumerate(items)},
                      room_id=room.get("id"), graph_selected=selected, surface="graph",
                      primary=("chat", primary["participant"]) if primary else None,
                      bands=bands_hit, band_offsets=scrolled_now)
    if not color and selected:
        previous = None
        for hit in hits:
            if hit["action"] != selected or navigation.get("box") and hit["y"] >= navigation["box"]["y1"]:
                continue
            if previous != hit["y"] - 1:
                # Card runs share an action; mark their first row, preserving other columns.
                line = padded(lines[hit["y"] - 1], width)
                columns = [0]
                for char in line:
                    columns.append(columns[-1] + display_width(char))
                first = next(i for i, column in enumerate(columns) if column >= hit["x1"] - 1)
                last = next(i for i, column in enumerate(columns) if column >= hit["x2"])
                left, cell, right = line[:first], line[first:last], line[last:]
                if not cell.rstrip().endswith("<"):
                    span = hit["x2"] - hit["x1"] + 1
                    border = cell[-1:] if cell.endswith(("╮", "+")) else ""
                    space = max(0, span - 2 - len(border))
                    if border:
                        end = max(i for i, column in enumerate(columns) if column <= hit["x1"] - 1 + space)
                        text = line[first:end]
                    else:
                        text = clipped(cell.rstrip(), space)
                    lines[hit["y"] - 1] = left + padded(text, space) + " <" + border + right
            previous = hit["y"]
    text = "\n".join(lines)
    # Plain-word separators keep one-cell ASCII stand-ins, so hit columns do not move.
    return text if unicode else text.translate({ord("·"): "/", ord("→"): ">", ord("×"): "x", ord("│"): "|"})


def tab_box(report, width, rows, navigation, color=False, unicode=True, *, members=None,
            primary=None, previewed=None, preview=None, teams=None, everyone=None, labels=None,
            main_attempt=None, automatic=False, claims=None, classify=None, past=None, attention=None):
    """The shared reading dock; hit rows are relative to the returned lines."""
    rows_high = rows if rows >= 3 else int(rows > 0)
    navigation["full_mail"] = None
    navigation.update(tab_strip_visible=bool(rows_high), tab_body_visible=rows_high >= 3)
    navigation.pop("box", None)
    if not rows_high:
        navigation.pop("full_result", None)
        return [], []
    room = mapping(report.get("room"))
    members = nodes(report) + native_advisors(report, nodes(report)) if members is None else members
    labels = main_names(members) if labels is None else labels
    mains = [m for m in members if m.get("role") == "main"]
    if primary is None:
        primary = next((m for m in mains if main_attempt in (m["participant"], m.get("attempt"))), next(iter(mains), None))
    if everyone is None:
        everyone = {m["participant"]: [c for c in members if c.get("role") != "main" and
                    c.get("parent") in (m["participant"], m.get("attempt"))] for m in mains}
    teams = everyone if teams is None else teams
    classify = call_classifier(report) if classify is None else classify
    if past is None:
        owners = {key for m in mains for key in (m["participant"], m.get("attempt")) if key}
        past = past_calls([c for calls in everyone.values() for c in calls] +
                          [m for m in members if m.get("role") != "main" and m.get("parent") not in owners], classify)
    # Calls settled before a rollover follow the newer ones; older rooms stay readable with room show --exact.
    shown = {action(c) for c in past}
    past = list(past) + [c for c in listing(room.get("predecessor_calls")) if action(c) not in shown]
    preview = mapping(navigation.get("preview")) if preview is None else preview
    if previewed is None:
        # Team rows include synthetic debate calls, which members never list.
        previewed = next((m for m in members + [c for calls in everyone.values() for c in calls] + past
                          if action(m) == preview.get("target")), None)
        if previewed is None and primary and rows_high >= 5 and not navigation.get("dismissed"):
            previewed, preview, automatic = primary, {"target": action(primary), "report": {}}, True
    if previewed and previewed.get("role") != "main" and not previewed.get("_native") and rows_high >= 3:
        navigation["full_result"] = action(previewed)
    else:
        navigation.pop("full_result", None)
    settled, unused = settler(report)
    unread_total = live_unread(report, members, settled)
    scrolled = mapping(navigation.get("band_offsets"))
    scrolled_now = dict(scrolled)
    lines, hits = [], []

    def paint(text, style):
        return PALETTE[style] + text + "\033[0m" if color and style else text

    def chosen(text, style):
        return PALETTE[style] + "\033[7m" + text + "\033[0m" if color else text

    def hue(member):
        return "codex" if member.get("provider") == "codex" else "main"

    def name_of(member):
        return labels.get(member["participant"]) or model_name(member)

    from panel_debate import signature, tab_body as debate_body
    from panel_messages import tab_body as messages_body
    top = 1
    from panel_view import attention_items
    attention = attention_items(report, unicode) if attention is None else attention
    active = navigation.get("tab") if navigation.get("tab") in dict(TABS) else "detail"
    if active == "needs" and not attention:
        active = "detail"
    if active == "detail" and rows_high >= 3 and previewed and previewed.get("role") == "main":
        from panel_messages import main_mail
        participant = previewed["participant"]
        # Pair totals also count result handoffs, so only mail-specific evidence enables the reader.
        if (main_mail(room, participant)
                or any(participant in listing(mapping(m).get("pending_for")) and not result_handoff(room, m)
                       for m in listing(room.get("pending_messages")))):
            navigation["full_mail"] = ("mail", previewed["participant"])
    navigation["main_attempt"] = main_attempt
    claims = claims_by_main(report) if claims is None else claims
    body, targets = detail(previewed, preview, width - 4, members, room, teams, everyone, labels,
                           claims=claims, classify=classify) if previewed else (
        ["No call selected · click a main or a call to see it here"], {})
    strip_only = rows_high < 3
    capacity = max(1, rows_high - 2)
    seen = navigation.setdefault("debate_seen", {})
    shown_main = primary["participant"] if primary else None
    digest = signature(report, shown_main)
    if active == "debate":
        seen[shown_main] = digest
    marks = {"debate": (" •" if unicode else " *") if digest and seen.get(shown_main) != digest else "",
             "messages": " %s unread" % unread_total if unread_total else "", "past": " %s" % len(past),
             "needs": " %s" % len(attention)}
    tabs = [entry for entry in TABS if (entry[0] != "needs" or attention) and (entry[0] != "past" or past)]
    if active not in dict(tabs):
        active = "detail"
    navigation["visible_tabs"] = [key for key, unused in tabs]
    abbreviations = {"detail": "Dtl", "plan": "Plan", "debate": "Dbt", "messages": "Msg", "past": "Past", "needs": "Attn"}
    for short in (0, 1, 2):
        gap = " " if short == 2 else "  "
        tiny = {"detail": "D", "plan": "P", "debate": "B", "messages": "M", "past": "H", "needs": "A"}
        texts = [(("[%s]" if short == 2 else "[ %s ]") if key == active else "%s") % (
            (name if short == 0 else abbreviations[key] if short == 1 else tiny[key]) +
            (marks.get(key, "") if short == 0 else " %s" % unread_total if key == "messages" and unread_total else
             " %s" % len(attention) if key == "needs" else ""))
            for key, name in tabs]
        if display_width(gap.join(texts)) <= width - 6:
            break
    edge = box_edge(gap.join(texts), width, unicode)
    column, span = 3, None
    for (key, unused), text in zip(tabs, texts):
        x1, x2 = column + 1, min(column + display_width(text), width - 3)
        if x1 <= x2:
            hits.append({"y": top, "x1": x1, "x2": x2, "action": ("tab", key)})
            span = (column, x2) if key == active else span
        column += display_width(text) + len(gap)
    role = previewed.get("role", "worker").upper() if previewed else ""
    style = (hue(previewed) if role == "MAIN" else "review" if role not in {"WORKER", "RESEARCHER"} else "worker") if previewed else "dim"
    lines.append(paint(edge, style) if span is None or not color else
                 paint(edge[:span[0]], style) + chosen(edge[span[0]:span[1]], style) + paint(edge[span[1]:], style))
    # No room for a body: the strip alone keeps every tab one click away.
    if strip_only:
        navigation.pop("box", None)
    else:
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
            plan = [(text, None) for text, unused in goal_box(report, width - 4, unicode, False)] + plan_rows(report, labels, width - 4, unicode)
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
        elif active == "past":
            rows = past_body(past, width - 4, capacity, navigation, labels, unicode)
        elif active == "needs":
            rows = attention_body(attention, width - 4, capacity, navigation, unicode)
        else:
            summary = [("Messages: %s unread for live participants" % unread_total, None, False)] if capacity >= 3 else []
            rows = summary + messages_body(report, width - 4, capacity - len(summary), navigation, unicode)
        previous_target = None
        for n in range(capacity):
            row = rows[n] if n < len(rows) else ("", None, False)
            text, target, picked = row[:3]
            # Attention rows are one per item, and neighbouring items may share an action, so only the picked row is marked.
            if not color and target and (picked if active == "needs" else target == navigation.get("selected")
                                         and target != previous_target):
                text = clipped(text, max(0, width - 6)) + " <"
            previous_target = target
            line = box_row(text, width, unicode)
            lines.append("\033[7m" + line + "\033[0m" if color and picked else line)
            if target:
                # Body text without its own link is passive, so a click that selects text changes nothing.
                hits.append({"y": top + 1 + n, "x1": 1, "x2": width, "action": target,
                             **({"preview": True, "passive": n in plain} if active == "detail" else
                                dict({active: True}, **{"key": row[3]} if len(row) > 3 else {})
                                if active in {"past", "needs"} else {})})
        lines.extend([box_row("", width, unicode)] * (rows_high - 2 - capacity))
        lines.append(paint(box_edge("", width, unicode, "bottom"), style))
        navigation["box"] = {"y1": top, "y2": top + rows_high - 1, "pages": bool(previewed) or active == "plan"}
    navigation["band_offsets"] = scrolled_now
    if not unicode:
        lines = [line.translate({ord("·"): "/", ord("→"): ">", ord("×"): "x", ord("│"): "|"}) for line in lines]
    return lines, hits


TABS = (("detail", "Detail"), ("plan", "Plan"), ("debate", "Debate"), ("messages", "Messages"), ("past", "Past"),
        ("needs", "Attention"))


def past_calls(pool, classify):
    """Settled calls of the shown pool, newest first; the board shows none of them.

    Callers pass the calls their board draws from, so an attention-only board empties the list as a whole."""
    unique = {action(c): c for c in pool if c.get("role") != "main" and classify(c) == "past"}
    return sorted(unique.values(), key=lambda c: (str(c.get("ended") or c.get("joined_at") or ""), c.get("seq", 0)),
                  reverse=True)


def past_body(past, width, capacity, navigation, labels, unicode):
    """One row per past call: who, its task, how it ended and when; selecting one shows it in Detail."""
    sep = " · " if unicode else " / "
    ids = [action(c) for c in past]
    navigation["past_ids"] = ids
    view = navigation.setdefault("past_view", {})
    header = capacity >= 3
    page = max(1, capacity - int(header))
    first = min(max(0, view.get("scroll", 0)), max(0, len(ids) - page))
    selected = navigation.get("selected")
    if view.pop("reveal", False) and selected in ids:
        at = ids.index(selected)
        first = at if at < first else max(first, at - page + 1)
    view.update(scroll=first, page=page)
    rows = [("%s past call%s, newest first%sEnter shows one in Detail" % (len(ids), "" if len(ids) == 1 else "s", sep),
             None, False)] if header else []
    for call in past[first:first + page]:
        who = labels.get(call.get("participant")) or "%s %s" % (ROLES.get(call.get("role"), "Worker"), model_name(call))
        end = clock_stamp(call["ended"]) if call.get("ended") else call.get("_finished") or ""
        words = end_words(call, said(call["state"]))
        title = clean(call.get("title")) or "no task title"
        # Narrow rows give up the role word, then the time, then the model before the task title: rows differ by task.
        model = model_name(call)
        for head, tail in ((who, (words, end)), (model, (words, end)), (model, (words,)), ("", (words,))):
            tail = sep.join(p for p in tail if p)
            space = width - display_width(head) - display_width(tail) - display_width(sep) * (bool(head) + bool(tail))
            if space >= 10:
                break
        text = sep.join(p for p in (head, fit(title, space) if space >= 1 else "", tail) if p)
        rows.append((clipped(text, width), action(call), action(call) == selected))
    return rows


def attention_body(ranked, width, capacity, navigation, unicode):
    """The Attention tab: the ranked queue under one heading per class; selecting an item works as in Past."""
    from panel_view import ATTENTION_CLASSES, attention_header, attention_row
    sep = " · " if unicode else " / "
    lines = []
    for kind, unused, unused_short, heading in ATTENTION_CLASSES:
        group = [item for item in ranked if item["cls"] == kind]
        if group:
            lines.append((clipped("%s (%s)" % (heading, len(group)) + (sep + "oldest first" if kind == "decide" else ""),
                                  width), None, None))
            lines += [("  " + attention_row(item, width - 2, unicode, tag=False), item["action"], item["row_id"])
                      for item in group]
    # Items may share an action (a main's question and its note both open its chat), so rows are picked by item.
    picks = [(target, key) for unused, target, key in lines if target]
    navigation["needs_ids"], navigation["needs_keys"] = [p[0] for p in picks], [p[1] for p in picks]
    view = navigation.setdefault("needs_view", {})
    header = capacity >= 3
    page = max(1, capacity - int(header))
    first = min(max(0, view.get("scroll", 0)), max(0, len(lines) - page))
    pick = past_pick(navigation, "needs")
    if view.pop("reveal", False) and pick is not None:
        at = next(n for n, line in enumerate(lines) if line[2] == pick)
        first = at if at < first else max(first, at - page + 1)
    view.update(scroll=first, page=page)
    title = attention_header(ranked, width, unicode)
    hint = title + sep + "Enter shows one in Detail"
    rows = [(hint if display_width(hint) <= width else title, None, False)] if header else []
    return rows + [(text, target, key is not None and key == pick, key) for text, target, key in lines[first:first + page]]


def past_pick(navigation, tab):
    """The key of the chosen row in a Past or Attention list: the last row picked while it still has the selection."""
    ids = navigation.get(tab + "_ids", [])
    keys, selected, pick = navigation.get(tab + "_keys", ids), navigation.get("selected"), navigation.get(tab + "_pick")
    if pick in keys and ids[keys.index(pick)] == selected:
        return pick
    return keys[ids.index(selected)] if selected in ids else None


def past_event(event, navigation, tab="past"):
    """Past- and Attention-tab keys: arrows pick a row, Enter or a click shows it in Detail, f opens it on the right."""
    ids, kind, selected = navigation.get(tab + "_ids", []), event[0], navigation.get("selected")
    keys, pick = navigation.get(tab + "_keys", ids), past_pick(navigation, tab)
    view = navigation.setdefault(tab + "_view", {})
    if kind in {"up", "down", "home", "end"}:
        if ids:
            at = keys.index(pick) if pick is not None else (-1 if kind == "down" else len(ids))
            at = 0 if kind == "home" else len(ids) - 1 if kind == "end" else \
                min(len(ids) - 1, max(0, at + (1 if kind == "down" else -1)))
            navigation["selected"], navigation[tab + "_pick"] = ids[at], keys[at]
            view["reveal"] = True
        return True
    if kind in {"scroll", "pageup", "pagedown"}:
        step = max(1, view.get("page", 1))
        view["scroll"] = max(0, view.get("scroll", 0) + (event[1] if kind == "scroll" else
                                                          -step if kind == "pageup" else step))
        return True
    if kind == "click":
        hit = next((h for h in navigation.get("hits", []) if h.get(tab) and h["y"] == event[2]
                    and h["x1"] <= event[1] <= h["x2"]), None)
        if hit is None:
            return False
        target = hit["action"]
        navigation[tab + "_pick"] = hit.get("key", target)
    elif kind in {"enter", "f"} and selected in ids:
        target = selected
    else:
        return False
    if target[0] == "tab":
        # A stale hold has no call left; its row opens the Plan tab that lists the task.
        navigation["tab"] = target[1]
        return True
    # The call reads exactly as a board card does: Detail shows it and f opens its full result on the right.
    navigation.update(selected=target, preview={"target": target}, tab="detail")
    navigation.pop("dismissed", None)
    if kind == "f" and target[0] in {"result", "debate"}:
        navigation["open_call"] = target
    return True


PLAN_ORDER = {"done": 0, "cancelled": 1, "review": 2, "landing": 2, "running": 3, "claimed": 3, "ready": 4, "blocked": 5}


def plan_rows(report, labels, width, unicode):
    """The shared repository plan's tasks, completed work first."""
    tasks = listing(mapping(report.get("room")).get("repo_tasks"))
    glyphs = (dict(done="✓", cancelled="×", review="◐", landing="◐", running="●", claimed="●", ready="○", blocked="✗") if unicode else
              dict(done="+", cancelled="x", review="~", landing="~", running="*", claimed="*", ready="o", blocked="x"))
    rows = []
    awaiting, ended = held_split(report)
    for t in sorted((mapping(t) for t in tasks), key=lambda t: PLAN_ORDER.get(t.get("state"), 5)):
        by = t.get("claimed_by_participant")
        word = "in review" if t.get("state") == "review" else clean(t.get("state"), 20) or "unknown"
        if t.get("state") == "running" and t.get("id") in ended | awaiting:
            word += " (worker ended)" if t["id"] in ended else " (awaiting decision)"
        if word == "done":
            kind = t.get("completion_kind")
            if kind == "research-accepted":
                word = "done (research)"
            elif kind == "main-research-accepted":
                word = "done (main research)"
            elif kind == "satisfied-by":
                word = "done (satisfied by)"
        locally_done = t.get("id") in mapping(report.get("completed_tasks")) and t.get("state") != "done"
        if locally_done:
            word = "completed locally · plan " + word
        age = t.get("claim_age_s")
        if not locally_done and t.get("claim_expired") and isinstance(age, (int, float)) and age >= 0:
            word += " %d%s stale" % ((age // 86400, "d") if age >= 86400 else (age // 3600, "h") if age >= 3600 else (age // 60, "m"))
        # A finished task has no owner or check left to act on, so those columns would only read as warnings.
        parts = [glyphs.get("done" if locally_done else t.get("state"), "?") + " " + clean(t.get("id"), 40),
                 word, clean(t.get("title"), 120) or "untitled"]
        extra = []
        if t.get("state") != "done" and not locally_done:
            extra = [clean(labels.get(by) or by, 40) if by else "unclaimed", clean(t.get("verify"), 80) or "no verify"]
        if width >= 100:
            parts += extra
        rows.append((" · ".join(parts), ("task", t.get("id")) if t.get("id") else None))
        if width < 100 and extra:
            rows.append(("  " + " · ".join(extra), None))
    return rows or [("No repository plan recorded", None)]


def action(member):
    return member.get("_action") or ("chat" if member.get("role") == "main" else "result", member["participant"])


def footer_hints(navigation, width, managed, unicode, member, graph):
    """Selection hints followed by fixed panel keys, with bounded help rows."""
    tabs = navigation.get("tab_strip_visible")
    selected = navigation.get("selected")
    kind = selected[0] if selected else "chat"
    arrows, move = ("←→", "↑↓") if unicode else ("<>", "^v")
    toggle = "F9 Chat⇄Board" if unicode else "F9 Chat<>Board"
    worktree = bool(member and kind == "result" and str(member.get("location") or "").startswith("worktree:"))
    mail = bool(navigation.get("full_mail") and navigation.get("tab", "detail") == "detail"
                and navigation.get("tab_body_visible") and not navigation.get("detail"))
    mail_hint = ("f Open right" if managed else "f Full text") if mail else ""
    message_call = tabs and navigation.get("tab") == "messages" and navigation.get("message_has_call")
    if navigation.get("keys_help"):
        keys = ["? Hide", "Esc Back", "F5 Control" if managed else "", "k Capacity", "u Refresh",
                ("Enter Collapse" if navigation.get("messages_view", {}).get("open") is not None and navigation.get("messages_view", {}).get("open") == navigation.get("messages_view", {}).get("selected") else "Enter Expand") if tabs and navigation.get("tab") == "messages" else
                "Enter Open right" if managed and kind in {"result", "debate"} else "Enter Open",
                move + " Move", "Space Fold", "a Ask advisor",
                "w Worktree" if worktree else "", mail_hint if mail else "f Open right" if message_call or managed and navigation.get("full_result") else
                "f Full result" if navigation.get("full_result") else "",
                "Tab Next tab" if tabs else "", "g Graph", "t Tree",
                "n New main" if managed and not navigation.get("main_occupied") else "",
                "v " + ("Collapse" if navigation.get("expanded") else "Expand"), "b Attention",
                "Shift-drag Copy" if managed else "", toggle if managed else "q Quit", "F12 Keys" if managed else ""]
        lines = [""]
        for key in [k for k in keys if k]:
            key = clipped(key, width)
            if lines[-1] and display_width(lines[-1] + "  " + key) > width:
                lines.append("")
            lines[-1] += ("  " if lines[-1] else "") + key
        return lines[:max(0, navigation.get("help_rows", 4))]
    # The dock falls back to Detail for a tab it does not show (an empty Past list, say); the hints follow the dock.
    tab = navigation.get("tab") if tabs and navigation.get("tab_body_visible", True) and not navigation.get("detail") and \
        navigation.get("tab") in navigation.get("visible_tabs", [navigation.get("tab")]) else None
    right = managed and (kind in {"result", "debate"} or navigation.get("preview") and
                        navigation["preview"].get("target", (None,))[0] in {"result", "debate"})
    expanded_message = navigation.get("messages_view", {}).get("open") == navigation.get("messages_view", {}).get("selected") and navigation.get("messages_view", {}).get("open") is not None
    contextual = (["Enter Collapse" if expanded_message else "Enter Expand", "f Open right", move + " Message", arrows + " Filter"] if tab == "messages" and navigation.get("message_has_call")
                  else [move + " Message", "Enter Collapse" if expanded_message else "Enter Expand", arrows + " Filter"] if tab == "messages"
                  else [move + " Seat", "Enter Open right" if managed else "Enter Open", arrows + " Target"] if tab == "debate" and not navigation.get("debate_empty")
                  else ["Tab Next tab"] if tab == "debate"
                  else [move + " Call", "Enter Plan" if kind == "tab" else "Enter Detail",
                        ("f Open right" if managed else "f Full result") if tab == "past" or kind in {"result", "debate"} else ""]
                  if tab in {"past", "needs"}
                  else ["wheel Scroll", "Tab Next tab"] if tab == "plan" else None)
    contextual = contextual or (["Esc Back to graph", "g Graph"] if navigation.get("group_tree") else []) + [
                  "Enter Open right" if right else "",
                  "f Open right" if managed and navigation.get("full_result") else "f Full result" if navigation.get("full_result") else "",
                  "Esc Close" if navigation.get("preview") or navigation.get("detail") else "",
                  "Enter Chat" if kind == "chat" else "Enter Fold" if kind == "group" else "" if right else "Enter Show",
                  "a Ask advisor" if kind == "chat" else "w Worktree" if worktree else "",
                  "Tab Next tab" if tabs and not navigation.get("detail") else "",
                  "Space Fold" if kind == "group" or kind == "chat" and not graph else ""]
    if navigation.get("reader_open"):
        contextual.insert(0, "Esc Close right")
    if not tabs:
        contextual.append("v Expand")
    contextual.sort(key=lambda hint: 0 if hint.startswith("Enter ") or hint.startswith("f ") or hint.startswith("Esc ") else 1)
    def pack(reserved):
        packed = []
        for hint in [h for h in contextual if h]:
            if len(packed) < 3 and display_width("  ".join(packed + [hint] + reserved)) <= width:
                packed.append(hint)
        if mail_hint and display_width("  ".join(packed + [mail_hint] + reserved)) <= width:
            packed.append(mail_hint)
        return packed

    reserved = [toggle, "F12 Keys"] if managed and width >= 64 else ["F9", "F12 Keys"] if managed else ["q Quit", "? Keys"]
    packed = pack(reserved)
    if managed and width < 64 and pack(["F9 Swap", "F12 Keys"]) == packed:
        # The short F9 label is shown only when no contextual hint loses its place to it.
        reserved = ["F9 Swap", "F12 Keys"]
    tail = "  ".join(reserved)
    return ["  ".join(packed + reserved)] if display_width(tail) <= width else [clipped(tail, width)]
