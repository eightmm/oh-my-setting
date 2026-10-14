"""Room graph drawn from participation, addressed messages and lifecycle evidence."""

import calendar
import re
import time

from dashboard_projection import ELLIPSIS, clean, count as valid_count, display_width, fit, listing, mapping
from panel_view import (ATTENTION_STATES, MODEL_NAMES, LIVE_STATES, TERMINAL_STATES, PALETTE, activity, box_edge, box_row,
                        clipped, location_label, main_occupied, menu_rows, status_alerts, worker_rows, wrapped,
                        role_style, call_identity, status_tag, paint, paint_card_edges, color_count, light_theme, glyph,
                        JUDGE_ROLES, WORKER_ROLES, swap_line, judge_band, band_style, role_legend,
                        judgment_tag, judgment_tone, call_ending, ending_tag, call_tone)


def room_ids(room):
    """This room and the sealed rooms it continues: attempts started before a rollover name one of them."""
    return ({room.get("id"), room.get("predecessor")} | set(listing(room.get("lineage")))) - {None}


def owed(report, call):
    """Whether a call settled in an earlier room still waits on the person: a decision, an admission or a held task."""
    task = call.get("task_id")
    if not task or mapping(report.get("finalized")).get(task) in {"completed", "accepted"}:
        return False
    running = {t.get("id") for t in map(mapping, listing(mapping(report.get("plan")).get("tasks")))
               if t.get("state") == "running"}
    # Until the snapshot knows the pending patches every finished write call is a candidate.
    pending = report.get("awaiting_admission")
    admitting = pending is None or call["participant"] in listing(pending)
    return (call["state"] in {"review", "blocked"} or task in running
            or call["state"] == "done" and call.get("access") == "write" and admitting)


def nodes(report):
    room = mapping(report.get("room"))
    attempts = mapping(report.get("attempts"))
    linked = {}
    rows = listing(attempts.get("active_recent")) + listing(attempts.get("recent"))
    here = room_ids(room)
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
    present = {mapping(m).get("participant") for m in listing(room.get("participants"))}
    # Calls settled in a room this one continues are not carried, yet some still wait on the person; only one with
    # attempt evidence has a task to wait on, which bounds the work however long the chain grows.
    known = (set(linked) | set(mapping(report.get("room_attempts")))) - present
    earlier = [c for c in map(mapping, listing(room.get("predecessor_calls"))) if c.get("participant") in known]
    for member in listing(room.get("participants")) + earlier:
        if not member.get("joined"):
            continue
        # Dispatched children inherit their main's participant as provenance;
        # only a same-role attempt is the member itself.
        attempt = next((row for row in linked.get(member.get("participant"), [])
                        if mapping(row.get("panel")).get("role") == member.get("role")), {})
        if not attempt:
            # Outside the projection window: the full-projection row for this call, when the snapshot found one.
            attempt = mapping(mapping(report.get("room_attempts")).get(member.get("participant")))
        meta = mapping(attempt.get("panel"))
        refs = mapping(attempt.get("refs"))
        extra = mapping(mapping(report.get("room_attempts")).get(member.get("participant")))
        if not attempt.get("attempt_id") or extra.get("attempt_id") != attempt.get("attempt_id") or extra.get("state") != attempt.get("state"):
            extra = {}
        reason = attempt.get("reason_code") or extra.get("reason_code") or member.get("reason_code")
        operation = next((r for r in listing(mapping(report.get("operations")).get("recent"))
                          if attempt.get("attempt_id") and r.get("attempt_id") == attempt["attempt_id"]
                          and r.get("task_id") == attempt.get("task_id") and r.get("kind") in {"delegate", "call", "ask", "review", "consult"}), {})
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
        result = ({"exit": member.get("exit"), "ts": member.get("ended")} if member.get("_predecessor") else
                  mapping(mapping(room.get("call_results")).get(member.get("participant"))))
        code = result.get("exit", attempt.get("exit", member.get("exit")))
        provider_error = attempt.get("provider_error_exit", refs.get("provider_error_exit", extra.get("provider_error_exit",
                            mapping(extra.get("refs")).get("provider_error_exit", member.get("provider_error_exit")))))
        if isinstance(provider_error, str):
            provider_error = int(provider_error) if re.fullmatch(r"\d{1,4}", provider_error) else None
        derived = "presence unknown" if member.get("role") == "main" or type(code) is not int else \
            "done" if code == 0 else "timed_out" if code == 124 else "failed"
        state = attempt.get("state") or derived
        # Display only: a review whose task its main completed or accepted reads "decided"; the state stays.
        decided = state == "review" and finalized.get(attempt.get("task_id")) in {"completed", "accepted"}
        output.append(dict(member, attempt=attempt.get("attempt_id"), state=state, _decided=decided,
                           panel_activity=mapping(mapping(report.get("main_activity")).get(member.get("participant"))),
                           ended=result.get("ts"), exit=code if type(code) is int else None,
                           artifact=result.get("artifact") or attempt.get("artifact") or member.get("artifact"),
                           reason_code=reason, guard=member.get("guard") is True or reason == "worker_authority_violation",
                           verify_exit=result.get("verify_exit", attempt.get("verify_exit", operation.get("verify_exit", member.get("verify_exit")))),
                           verify_status=result.get("verify_status", attempt.get("verify_status", member.get("verify_status"))),
                           provider_error_exit=provider_error if type(provider_error) is int else None,
                           model=model, model_at_launch=meta.get("model") or member.get("model"),
                           task_id=attempt.get("task_id"), decision=finalized.get(attempt.get("task_id")), location=meta.get("location"),
                           lease_id=meta.get("lease_id") or attempt.get("lease_id"),
                           effort=effort, purpose=clean(meta.get("purpose") or meta.get("strategy"), 60),
                           access=meta.get("access") or attempt.get("access"),
                           harness=mapping(attempt.get("panel")).get("harness"),
                           title=member.get("label_override") or meta.get("label") or member.get("label")))
        if member.get("_predecessor") and not owed(report, output[-1]):
            output.pop()
    # Seq numbers restart in every room: calls launched in the rooms this one continues come first, the oldest room
    # first, a carried call by the room it was launched in.
    depth = {old: -n for n, old in enumerate(listing(room.get("lineage")), 1)}
    origins = mapping(room.get("call_origins"))
    order = lambda m: tuple(origins.get(m["participant"]) or (
        depth.get(m["_predecessor"], -1) if m.get("_predecessor") else 0, m.get("seq", 0)))
    # A continued worker shares its task with the earlier rounds of the same main.
    rounds = {}
    for member in sorted(output, key=order):
        if member.get("role") == "worker" and member.get("task_id") and member.get("parent"):
            earlier = rounds.setdefault((member["parent"], member["task_id"]), [])
            member.update(round=len(earlier) + 1, continues=earlier[-1] if earlier else None)
            earlier.append(member["participant"])
    # A later call of the same role on the same task supersedes an earlier failure; it is no longer attention.
    latest = {}
    for member in sorted(output, key=order):
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
        if reading.get("interrupted") is True or (reading.get("state") == "busy"
                                                   and reading.get("origin") == "user"):
            return "manual"
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
    """(percent used, 'dim'|None|'alert') for this main's collected context reading."""
    if member.get("role") != "main":
        return None
    row = mapping(mapping(report.get("provider_status")).get(member.get("provider")))
    if row.get("participant") != member.get("participant"):
        return None
    from panel_metrics import percent
    left = percent(row.get("context_left"))
    used = None if left is None else 100 - left
    return None if used is None else (used, "alert" if used >= 85 else "dim" if used < 70 else None)


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
                   if mapping(r).get("read") is True} | {
        "result-" + str(c.get("participant")) for c in map(mapping, listing(room.get("predecessor_calls")))
        if c.get("read") is True}


def closed_tasks(report):
    """Task ids the plan records done or cancelled, or whose owner completed or accepted them."""
    plan = {t.get("id") for t in map(mapping, listing(mapping(report.get("plan")).get("tasks")))
            if t.get("state") in {"done", "cancelled", "retired", "superseded"}}
    return {ident for ident in plan if ident} | {ident for ident, outcome in mapping(report.get("finalized")).items()
                                                  if outcome in {"completed", "accepted"}}


def failure_handled(report, call, read_results=None, closed=None):
    """A handoff, later round or closed task settles failures; guard stops need an owner decision."""
    read_results = handled_results(mapping(report.get("room"))) if read_results is None else read_results
    closed = closed_tasks(report) if closed is None else closed
    if call_ending(call) == "guard_stop":
        return bool(call.get("task_id") and (call.get("task_id") in closed or
                    mapping(report.get("finalized")).get(call["task_id"]) in {"completed", "accepted", "failed", "rejected"}))
    return ((call_ending(call) in {"failed", "timed_out", "verify_failed", "provider_error"} or
             call["state"] in ATTENTION_STATES - {"waiting_input", "waiting_approval", "review", "blocked"})
            and ("result-" + str(call.get("participant")) in read_results or call.get("superseded") is True
                 or bool(call.get("task_id")) and call.get("task_id") in closed))


def settler(report):
    """(settled, aged) for calls: finished, aged-out or handled calls leave the board; shared with the tree."""
    room = mapping(report.get("room"))
    read_results = handled_results(room)
    finalized = mapping(report.get("finalized"))
    closed = closed_tasks(report)
    attempts_seen = mapping(report.get("attempts"))
    plan_tasks = {t.get("id"): t for t in map(mapping, listing(mapping(report.get("plan")).get("tasks")))
                  if t.get("id")}
    members = nodes(report)
    mains = {key: m for m in members if m.get("role") == "main"
             for key in (m.get("participant"), m.get("attempt")) if key}
    # Only complete active evidence permits hiding an old unknown; it does not prove completion.
    complete = attempts_seen.get("available") is True and valid_count(attempts_seen.get("active")) is not None and (
        valid_count(attempts_seen.get("active")) <= len(listing(attempts_seen.get("active_recent"))))

    def aged(call):
        # The existing ten-minute display cutoff never changes the unknown lifecycle state.
        return complete and (joined_seconds(call) or 0) > 600

    def settled(call):
        # Finished or aged-out calls, and failed ones whose result its main has read, leave the board; the owner's
        # recorded decision on the task settles its failed, review and blocked calls, reading a result settles no review.
        # A completed or accepted outcome closes the task's calls; a failed outcome closes only calls that already
        # ended in failure, while its review and block stay for the rework the decision asks for.
        outcome = finalized.get(call.get("task_id"))
        # rejected withdraws the work: its review and block close too, unlike failed, which asks for rework.
        decided = outcome in {"completed", "accepted", "rejected"} or (
            outcome == "failed" and call["state"] in {"failed", "timed_out", "abandoned", "orphaned"})
        ending = call_ending(call)
        if ending == "cancelled":
            return True
        if ending in {"guard_stop", "verify_failed", "timed_out", "provider_error"}:
            return failure_handled(report, call, read_results, closed) or decided
        task_id = call.get("task_id")
        review = call["state"] == "review" or call["state"] == "done" and call.get("participant") in listing(report.get("awaiting_admission"))
        task = plan_tasks.get(task_id)
        owner = mains.get(call.get("parent"))
        owner_exited = owner is not None and main_state(owner) == "exited"
        claimant = task.get("claimed_by") or task.get("claimed_by_participant") if task else None
        successor = mains.get(claimant)
        adopted = (successor is not None and successor is not owner and successor.get("attempt")
                   and successor.get("state") not in TERMINAL_STATES)
        unowned_old = review and task is None and (joined_seconds(call) or 0) > 12 * 60 * 60
        handed_off = review and owner_exited and not adopted
        finished = call["state"] in FINISHED and not (
            call["state"] == "done" and call.get("participant") in listing(report.get("awaiting_admission")))
        return (finished or call["state"] == "presence unknown" and aged(call)
                or failure_handled(report, call, read_results, closed) or decided and call["state"] in ATTENTION_STATES - {"waiting_input", "waiting_approval"}
                or review and (task_id in closed or unowned_old or handed_off
                               # a later --continue round of the same task carries the decision now
                               or call.get("superseded") is True))
    return settled, aged


def call_classifier(report):
    """Display buckets only; admission and lifecycle evidence retain their own authority."""
    settled, _ = settler(report)
    awaiting = set(listing(report.get("awaiting_admission")))
    # Only a call with a main to sit above can linger there.
    mains = {key for m in nodes(report) if m.get("role") == "main" for key in (m["participant"], m.get("attempt")) if key}

    def classify(call):
        ending = call_ending(call)
        if ending == "cancelled":
            return "past"
        if ending == "guard_stop" and not settled(call):
            return "live"
        if call["state"] == "review" and not settled(call) or (
                call["state"] == "done" and call.get("participant") in awaiting and not settled(call)):
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
    ending = call_ending(call)
    if ending:
        words = ending.replace("_", " ")
        if ending in {"failed", "provider_error"} and type(call.get("exit")) is int:
            words += " (exit %s)" % call["exit"]
        return words
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
    A recorded lease must match the current task lease; otherwise old calls cannot hold a refreshed claim.
    Legacy rows without leases retain the claim-time fallback. No matching call, a live call or an unknown
    call stays claimed; a claim never proves a process."""
    admission = set(listing(report.get("awaiting_admission")))
    members = nodes(report) if members is None else members
    classify = call_classifier(report)
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
        lease = task.get("lease_id")
        states = ["review" if (m.get("state") == "done" and m.get("participant") in admission
                                or m.get("state") == "review") and classify(m) == "review"
                  else "done" if m.get("state") in {"review", "done"} else m.get("state")
                  for m in calls.get(task.get("id"), [])
                  if lease and m.get("lease_id") == lease
                  and (claimant and m.get("participant") == claimant or not claimant
                       or owners.get(m.get("parent"), m.get("parent")) == owners.get(claimant, claimant))
                  or not lease and not m.get("lease_id")
                  and (classify(m) == "review" or claimant and m.get("participant") == claimant or
                       (not claimant or owners.get(m.get("parent"), m.get("parent")) == owners.get(claimant, claimant))
                       and not (since and m.get("joined_at") and str(m["joined_at"]) < since))]
        if not states:
            # No call carrying the current lease is not evidence the work stopped: the claim stays claimed.
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
    goal_text = str(plan.get("goal") or "")
    goal = clean(goal_text, max(1, len(goal_text)))
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
    """Full goal on the board; bounded summaries retain the compact fallback."""
    if compact or width < 76:
        summary = plan_summary(report)
        goal = summary[0] if summary else "No goal recorded"
        counts = "%s/%s verified" % summary[1:3] if summary else ""
        text = "Goal / " + goal + (" / " + counts if counts else "")
        return [(one_row(text, width), "metadata")]
    return goal_box(report, width, unicode, compact)


def one_row(text, width):
    text = clipped(text, max(1, width - 4))
    return "[ " + text + " " * max(0, width - display_width(text) - 4) + " ]"


def goal_box(report, width, unicode, compact=False):
    summary = plan_summary(report)
    goal = summary[0] if summary else "No goal recorded"
    counts = "%s/%s verified" % summary[1:3] if summary else ""
    locally_done = sum(1 for task in listing(mapping(report.get("room")).get("repo_tasks"))
                       if task.get("state") != "done" and task.get("id") in mapping(report.get("completed_tasks")))
    if locally_done:
        counts += " / %s locally completed" % locally_done
    if summary:
        counts += "".join(" / %s %s" % (n, label) for n, label in zip(summary[3:], ("in review", "claimed", "stale")) if n)
    title = "Goal"
    right = clipped(counts, max(1, width - 14))
    edge = box_edge(title, width, unicode)
    if right:
        prefix, closing = edge[:8], edge[-2:]
        edge = prefix + ("─" if unicode else "-") * max(0, width - display_width(prefix + closing + right) - 2) + " " + right + " " + closing
    parts = wrapped(goal, max(1, width - 4), max(1, len(goal)), max_chars=max(1, len(goal)))
    if summary and summary[-1]:
        note = "%s stale, not counted: no live worker / release with oms agent-plan release --id ID" % summary[-1]
        parts += wrapped(note, max(1, width - 4), len(note))
    return [(edge, "border")] + [
        (box_row(line, width, unicode), None) for line in parts] + [
        (box_edge("", width, unicode, "bottom"), "border")]


def execution_tag(member, unicode=True):
    return ending_tag(dict(member, state=main_state(member)), unicode)


def member_identity(member, label=True):
    purpose = member.get("purpose")
    role = purpose if purpose in JUDGE_ROLES | WORKER_ROLES else member.get("role")
    if member.get("role") == "main":
        role = "main"
    title = member.get("title") if label else None
    if title in {"Native task not recorded", "Task unrecorded"}:
        title = None
    return call_identity(role, member.get("model"), title, member.get("_short_id"))



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


def layout(width):
    """(width, left column) of the centered main card."""
    span = max(12, width - 12)
    return span, (width - span) // 2


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
    counts = dict.fromkeys(("attention", "active", "awaiting decision", "finished", "cancelled", "unrecorded"), 0)
    for call in calls:
        state = call.get("state")
        if call_ending(call) == "cancelled":
            bucket = "cancelled"
        elif classify and classify(call) == "review":
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


def session_log(report, width, rows, unicode=True):
    """Recorded events only, deduplicated by message identity, in spare rows."""
    if rows < 3:
        return []
    room = mapping(report.get("room"))
    events, seen = [], set()
    people = {m["participant"]: m for m in nodes(report)}
    for message in listing(room.get("messages")):
        ident = message.get("id")
        if not ident or ident in seen:
            continue
        seen.add(ident)
        events.append(message)
    if not events:
        return []
    output = [(box_edge("Session log", width, unicode), None, "border")]
    for message in events[-min(5, rows - 2):]:
        member = people.get(message.get("sender"), {"role": "unknown"})
        identity = member_identity(member, False)
        role, model = identity.split(" ", 1)
        actor = "[" + role.lower() + " " + model + "]"
        try:
            moment = time.localtime(calendar.timegm(time.strptime(str(message.get("ts")), "%Y-%m-%dT%H:%M:%SZ")))
            stamp = time.strftime("%H:%M:%S" if moment[:3] == time.localtime()[:3] else "%m-%d %H:%M:%S", moment)
        except ValueError:
            stamp = "unknown"
        left = stamp + " " + actor + " "
        result_tag = judgment_tag(member, report, unicode, execution_tag(member, unicode))
        outcome = (result_tag if result_handoff(room, message) and call_ending(member) else
                   "[" + clean(message["outcome"], 30) + "]" if message.get("outcome") else "")
        source_text = str(message.get("text") or "")
        if result_handoff(room, message) and call_ending(member):
            words = result_tag[1:-1] if judge_band(member.get("role"), member.get("purpose")) else end_words(member, "finished")
            source_text = re.sub(r"^Call exit=\d+; parent acceptance pending\.?", words + ".", source_text)
        text = clipped(readable(source_text, " ") or "(empty)", max(1, width - 6 - display_width(left + outcome)))
        plain = left + text + " " * max(2, width - 4 - display_width(left + text + outcome)) + outcome
        output.append((box_row(plain, width, unicode), actor, role_style(member.get("role"), member.get("purpose"))))
    output.append((box_edge("", width, unicode, "bottom"), None, "border"))
    return output


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
    from panel_view import use_unicode
    use_unicode(unicode)
    if height < 16:
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation, managed=managed, previous=previous)
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
    # A held current main (blocked, waiting, presence unknown) still leads any ended predecessor.
    primary = owners.get(main_attempt) or next((m for m in mains if m["state"] in LIVE_STATES), next(
        (m for m in mains if m["state"] not in TERMINAL_STATES), next(iter(mains), None)))
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
        # Judges precede workers in the team lists the dock shows; the board tree orders by state.
        return sorted(calls, key=lambda m: (m.get("role") in {"worker", "researcher"},) + call_order(m))

    everyone = {m["participant"]: team(m, every=True) for m in mains}
    if navigation.get("group_tree"):
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation, managed=managed, previous=previous)
    all_calls = [c for calls in everyone.values() for c in calls]
    unlinked = [m for m in members if m.get("role") != "main" and m.get("parent") not in owners]
    if attention_only:
        unlinked = [m for m in unlinked if classify(m) != "past" and (m["state"] in ATTENTION_STATES or classify(m) == "review")]
    all_calls += unlinked
    children = [c for c in everyone.get(primary["participant"], []) if classify(c) != "past"] if primary else []
    hidden_calls = [c for c in everyone.get(primary["participant"], []) if not c.get("_native")] if primary else []
    compact = height < 20 or mapping(report.get("collection")).get("ok") is not True or room.get("error")
    folded_calls = hidden_calls if interactive and compact and not attention_only and not children else []
    children.sort(key=lambda m: (m["state"] not in LIVE_STATES, call_order(m)[1:]))
    judges = [c for c in children if role_style(c.get("role"), c.get("purpose")) == "judge"]
    gates = [c for c in judges if judge_band(c.get("role"), c.get("purpose")) == "gate"]
    counsel = [c for c in judges if c not in gates]
    workers = [c for c in children if c not in judges]
    selected_member = next((c for c in members + all_calls if action(c) == selected), None)
    preview = mapping(navigation.get("preview"))
    previewed = next((c for c in members + all_calls if action(c) == preview.get("target")), None)
    automatic = not previewed and not navigation.get("dismissed") and primary is not None
    if automatic:
        previewed, preview = primary, {"target": action(primary), "report": {}}
    navigation["cancel_owner"] = {"participant": owners[main_attempt]["participant"], "attempt": main_attempt} if main_attempt in owners else None
    navigation["main_attempt"] = main_attempt
    navigation["full_mail"] = None
    navigation.pop("full_result", None)
    claims = claims_by_main(report)
    settled, _ = settler(report)
    unread_total = live_unread(report, members, settled)
    from panel_view import attention_items, attention_header, tone
    from panel_metrics import percent
    ranked = attention_items(report, unicode)
    lines, hits, bands = [], [], []
    colors, light = color_count(), light_theme()

    def styled(text, style_key="text", bold=False):
        return paint(text, style_key, color=color, colors=colors, light=light, bold=bold)

    def add(text, style_key="text", bold=False):
        lines.append(styled(clipped(text, width), style_key, bold))

    import os
    repo = mapping(report.get("repo"))
    title = "OMS · " + (os.path.basename(clean(repo.get("name")) or str(repo.get("path") or "").rstrip("/\\")) or "repository")
    clock = time.strftime("%m-%d ") + WEEKDAYS[time.localtime().tm_wday] + time.strftime(" %H:%M")
    room_clock = width - display_width(title) - display_width(clock)
    lines.append(styled(clipped(title, width), bold=True) + (" " * room_clock + styled(clock, "metadata") if room_clock >= 2 else ""))
    legend = "  ".join(text for text, _ in role_legend(unicode))
    legend_state = "  ~ running  + verified  ! attention"
    if room.get("id") and display_width(legend + legend_state) <= width:
        legend += legend_state
    # Role swatches carry only role colour, independent of execution state.
    for text, style_key in role_legend(unicode):
        legend = legend.replace(text, styled(text, style_key))
    lines.append(legend)
    hints = menu_rows(width, 1, unicode, navigation.get("menu_help", False), managed,
                      occupied=navigation["main_occupied"], installed=navigation.get("installed")) if menu else []
    footer_extra = []
    if menu and previous:
        from panel_view import message_rows
        footer_extra = message_rows("Last: " + previous, width) if "\n" in previous or "oms panel --release-main " in previous else [clipped("Last: " + previous, width)]
    if navigation.get("notice"):
        footer_extra.append(clean(navigation["notice"], 180))
    bar = None if menu else spawn_bar(width, navigation)
    if bar:
        footer_extra.append(bar[0])
    help_rows = len(footer_hints(navigation, width, managed, unicode, selected_member, True)) if navigation.get("keys_help") and not menu else 1
    total_rows = max(0, height - int(menu) - 1 - help_rows - len(footer_extra))
    # Detail releases its body when the right reader already owns that call.
    active = navigation.get("tab", "detail")
    reader_target = navigation.get("reader_target") if navigation.get("reader_open") else None
    same_reader = reader_target and previewed and reader_target == action(previewed)
    dock_body = dock_body_rows(height)
    if active == "detail" and reader_target:
        dock_body = 0 if same_reader else 1
    dock_rows = dock_body + 2 if dock_body else 1
    main_rows = 5 if height >= 28 else 4 if height >= 20 else 3
    minimum_tree = min(2, len(gates)) + min(2, len(counsel)) + min(2, len(workers)) or int(bool(folded_calls))
    goal_rows = goal_box(report, width, unicode)
    goal_cap = max(3, total_rows - len(lines) - min(dock_rows, 4) - main_rows - minimum_tree - 1)
    goal_overflow = len(goal_rows) > goal_cap
    if goal_overflow:
        parts = goal_rows[1:-1]
        count = max(1, goal_cap - 3)
        start = min(max(0, mapping(navigation.get("band_offsets")).get("goal", navigation.get("goal_offset", 0))), max(0, len(parts) - count))
        goal_rows = goal_rows[:1] + parts[start:start + count] + [(box_row("Goal lines %s-%s/%s / full goal in Plan / wheel" % (
            start + 1, start + count, len(parts)), width, unicode), "metadata")] + goal_rows[-1:]
    goal_top = len(lines) + 1
    for text, style_key in goal_rows:
        hits.append({"y": len(lines) + 1, "x1": 1, "x2": width, "action": ("tab", "plan")})
        add(text, style_key or "text")
    if goal_overflow:
        bands.append({"name": "goal", "y1": goal_top, "y2": len(lines), "x1": 1, "x2": width})
    if room.get("swaps") and height >= 24:
        add(swap_line(room), "metadata")
    if report.get("swap_state"):
        add("[! swapping main] " + clean(report["swap_state"], 80), "alert")
    unavailable = mapping(report.get("collection")).get("ok") is not True or room.get("error")
    strip = "! State unavailable" + (" / " + clean(room.get("error"), 120) if room.get("error") else "") + " / u Refresh" if unavailable else attention_header(ranked, width, unicode)
    add(strip, "bad" if unavailable else "alert" if ranked else "metadata")
    hits.append({"y": len(lines), "x1": 1, "x2": width, "action": ("tab", "needs")})
    if total_rows - len(lines) - dock_rows - main_rows < minimum_tree:
        dock_rows = max(1, total_rows - len(lines) - main_rows - minimum_tree)
        if dock_rows == 2:
            dock_rows = 1
    remaining = max(0, total_rows - len(lines) - dock_rows - main_rows)
    expansion = 3 if selected_member and selected_member in children and remaining >= minimum_tree + 3 else 0
    remaining -= expansion
    groups = (("gate", gates), ("counsel", counsel), ("calls", workers))
    allocation = dict.fromkeys((name for name, _ in groups), 0)
    for name, group in groups:
        # Two rows keep a band's first call beside its "N more" line.
        allocation[name] = min(len(group), 2, max(0, remaining - sum(allocation.values())))
    while sum(allocation.values()) < remaining:
        changed = False
        for name, group in groups:
            if allocation[name] < len(group) and sum(allocation.values()) < remaining:
                allocation[name] += 1
                changed = True
        if not changed:
            break
    main_width, main_left = layout(width)
    stem = main_left + 2
    offsets = dict(mapping(navigation.get("band_offsets")))
    drawn = []

    awaiting_admission = set(listing(report.get("awaiting_admission")))

    def tagged(member):
        # A finished write call with an unadmitted patch says so; "done" alone would hide the decision it needs.
        if member.get("state") == "done" and member.get("participant") in awaiting_admission:
            return status_tag("review", unicode=unicode).replace("awaiting decision", "patch awaits admission")
        return judgment_tag(member, report, unicode, execution_tag(member, unicode))

    def card_lines(member, content, indent=0, tree_card=False, band_prefix=""):
        style_key = band_style(member.get("role"), member.get("purpose"))
        identity = band_prefix + member_identity(member)
        mark = glyph("selected", unicode) + " " if action(member) == selected else ""
        card_width, card_left = main_width - indent, main_left + indent
        block = [box_edge(mark + identity, card_width, unicode, dashed=True)]
        block += [box_row(text, card_width, unicode, dashed=True) for text in content]
        block += [box_edge("", card_width, unicode, "bottom", dashed=True)]
        for n, cell in enumerate(block):
            if n in (0, len(block) - 1):
                cell = styled(cell, style_key, n == 0)
            else:
                cell = paint_card_edges(cell, style_key, unicode=unicode, color=color, colors=colors, light=light)
                tag = tagged(member)
                if tag in cell:
                    cell = cell.replace(tag, styled(tag, judgment_tone(tag, call_ending(member) or member.get("state"))))
            lines.append(" " * card_left + cell + " " * max(0, width - card_left - card_width))
            hits.append({"y": len(lines), "x1": 1 if tree_card and n == 0 else card_left + 1,
                         "x2": width if tree_card and n == 0 else card_left + card_width,
                         "action": action(member), "preview": True,
                         **({"tree": True, "expanded": True} if tree_card and n == 0 else {})})

    def tree(group, name, slots):
        if not group or not slots:
            return
        visible = min(len(group), slots - int(slots < len(group)))
        visible = max(1, visible)
        start = min(max(0, offsets.get(name, 0)), max(0, len(group) - visible))
        index = next((i for i, m in enumerate(group) if action(m) == selected), None)
        if index is not None and selected != navigation.get("graph_selected"):
            start = min(start, index) if index < start else max(start, index - visible + 1)
        shown = group[start:start + visible]
        if index is not None and group[index] not in shown:
            shown = [group[index]] + shown[1:] if index < start else shown[:-1] + [group[index]]
        hidden = [c for c in group if c not in shown]
        first = len(lines) + 1
        for n, member in enumerate(shown):
            style_key = band_style(member.get("role"), member.get("purpose"))
            connector = ("└╌ " if n == len(shown) - 1 and not hidden else "├╌ ") if unicode else ("`- " if n == len(shown) - 1 and not hidden else "+- ")
            mark = glyph("selected", unicode) + " " if action(member) == selected else "  "
            band_prefix = (glyph(name, unicode) + " " + (name.title() + " " if n == 0 else "")) if name != "calls" else ""
            tag = tagged(member)
            available = max(0, width - stem - display_width(connector + mark))
            tail = clipped(tag + "  " + (elapsed(member.get("joined_at")) or "unknown"), max(0, available - 3))
            label = clipped(band_prefix + member_identity(member), max(1, width - stem - display_width(connector + mark + tail) - 2))
            gap = max(2, width - stem - display_width(connector + mark + label + tail))
            lines.append(" " * stem + styled(connector, style_key) + mark + styled(label, style_key, action(member) == selected) +
                         " " * gap + styled(tail[:len(tag)], judgment_tone(tag, call_ending(member) or member.get("state"))) + styled(tail[len(tag):], "metadata"))
            hits.append({"y": len(lines), "x1": 1, "x2": width, "action": action(member), "preview": True, "tree": True})
            drawn.append(member)
            if expansion and member is selected_member:
                lines.pop()
                hits.pop()
                card_lines(member, [tagged(member) + "  " + (elapsed(member.get("joined_at")) or "unknown"),
                                    "Task: " + clean(member.get("title"))], indent=4, tree_card=True, band_prefix=band_prefix)
        if hidden and slots > 1:
            after = [m for m in group[start + visible:] if m in hidden]
            add(padded(" " * stem + ("└╌ " if unicode else "`- ") + glyph("ellipsis", unicode) + " %s more %s / scroll" % (
                len(hidden), name if name != "calls" else "workers"), width), "metadata")
            hits.append({"y": len(lines), "x1": 1, "x2": width, "action": action((after or hidden)[0]), "preview": True, "tree": True})
        bands.append({"name": name, "y1": first, "y2": len(lines), "x1": 1, "x2": width})
        offsets[name] = start

    tree(gates, "gate", allocation["gate"])
    tree(counsel, "counsel", allocation["counsel"])
    if primary:
        tag = tagged(primary)
        activity_note = main_words(primary, unicode)
        status_text = tag + " / " + activity_note
        ctx = context_note(report, primary)
        context = "ctx %s%% used" % ctx[0] if ctx else ""
        status = clipped(status_text, main_width - 4) + " " * max(2, main_width - 4 - display_width(status_text + context)) + context if context else status_text
        question_count = open_count(room, primary["participant"])
        if question_count:
            status += " / ? %s open" % question_count
        held = claims.get(primary["participant"], [])
        awaiting_tasks, ended_tasks = held_split(report, members)
        current = [t for t in held if t.get("id") not in awaiting_tasks | ended_tasks]
        lead = (current or held)[0] if held else {}
        task = primary.get("task_id") or lead.get("id") or primary.get("title") or "no status yet"
        if task == "Native task not recorded":
            task = "no status yet"
        if lead:
            word = "awaiting decision" if lead.get("id") in awaiting_tasks else "worker ended" if lead.get("id") in ended_tasks else "in review" if lead.get("state") == "review" else clean(lead.get("state"))
            task += " · " + word
            if len(current) > 1:
                task += " · +%s more running" % (len(current) - 1)
            waiting = sum(t.get("id") in awaiting_tasks and t is not lead for t in held)
            if waiting:
                task += " · %s awaiting decision" % waiting
        content = [status, "Task: " + clean(task)]
        latest = declared_status(room, primary["participant"], main_width - 4)
        latest_text = " ".join(latest) if latest else "No latest statement recorded"
        stale = " (stale)" if any("(stale)" in line for line in latest) else ""
        def statement(span):
            return clipped(latest_text.replace(stale, "") if stale else latest_text, max(1, span - len(stale))) + stale
        if main_rows >= 5:
            content.append(statement(main_width - 4))
        elif main_rows == 4 and latest:
            task_span = max(10, (main_width - 7) // 2)
            content[1] = clipped(content[1], task_span) + " / " + statement(main_width - 7 - task_span)
        if main_rows == 3:
            content = [status + " / Task: " + clean(task)]
        card_lines(primary, content)
    else:
        for text in card("Main", ["No main connected / n starts a main"], main_width, main_rows, unicode):
            add(" " * main_left + text, "metadata")
    tree(workers, "calls", allocation["calls"])
    if folded_calls:
        add(" " * stem + glyph("ellipsis", unicode) + " %s more calls / Enter Detail" % len(folded_calls), "metadata")
        hits.append({"y": len(lines), "x1": 1, "x2": width, "action": action(folded_calls[0]), "preview": True, "tree": True})
    warnings = []
    if unlinked:
        open_unlinked = [c for c in unlinked if classify(c) != "past"]
        if open_unlinked:
            warnings.append("%s call%s with no known main%s / t shows them" % (len(open_unlinked),
                "s" if len(open_unlinked) != 1 else "", " (+%s past)" % (len(unlinked) - len(open_unlinked)) if len(open_unlinked) < len(unlinked) else ""))
    stranded = [c for m in mains if m is not primary and m["state"] in TERMINAL_STATES for c in everyone[m["participant"]] if classify(c) != "past"]
    if stranded:
        warnings.append("%s call%s of an ended main / t shows them" % (len(stranded), "s" if len(stranded) != 1 else ""))
    others = listing(room.get("other_rooms"))
    if others or room.get("other_rooms_incomplete"):
        warnings.append("OTHER ROOMS / " + " / ".join(clean(r.get("id"), 24) for r in others[:3]) +
                        (" / scan incomplete" if room.get("other_rooms_incomplete") else ""))
    spare = max(0, total_rows - dock_rows - len(lines))
    if warnings and spare:
        add(" / ".join(warnings), "alert")
        spare -= 1
    for text, actor, style_key in session_log(report, width, spare, unicode):
        lines.append(text.replace(actor, styled(actor, style_key)) if actor else styled(text, style_key))
    navigation["band_offsets"] = offsets
    dock_rows = min(dock_rows, max(0, total_rows - len(lines)))
    # Every tab keeps one fixed height at the bottom; rows the log did not use stay above it.
    lines += [""] * max(0, total_rows - len(lines) - dock_rows)
    navigation.update(tab_strip_visible=bool(dock_rows), tab_body_visible=dock_rows >= 3)
    if dock_rows:
        top = len(lines)
        dock_lines, dock_hits = tab_box(report, width, dock_rows, navigation, color, unicode, members=members,
            primary=primary, previewed=previewed, preview=preview, everyone=everyone, teams=everyone, labels=labels,
            main_attempt=main_attempt, automatic=automatic, claims=claims, classify=classify, attention=ranked,
            past=past_calls(all_calls, classify))
        lines.extend(dock_lines)
        hits.extend(dict(hit, y=hit["y"] + top) for hit in dock_hits)
        if navigation.get("box"):
            navigation["box"] = {**navigation["box"], "y1": navigation["box"]["y1"] + top, "y2": navigation["box"]["y2"] + top}
            bands.append(dict(navigation["box"], name="detail", x1=1, x2=width, step=3))
    lines += [""] * max(0, total_rows - len(lines))
    footer = "Workers %s/%s / Judges %s/%s / Mail %s" % (
        sum(c["state"] in LIVE_STATES for c in all_calls if role_style(c.get("role"), c.get("purpose")) == "worker"),
        sum(role_style(c.get("role"), c.get("purpose")) == "worker" for c in all_calls),
        sum(c["state"] in LIVE_STATES for c in all_calls if role_style(c.get("role"), c.get("purpose")) == "judge"),
        sum(role_style(c.get("role"), c.get("purpose")) == "judge" for c in all_calls), unread_total)
    if unavailable:
        footer = "Workers ?/? / Judges ?/? / Mail ?"
    health = status_alerts(report)
    land = mapping(report.get("land"))
    condition = "State unavailable" if unavailable else " / ".join(health) if health else ""
    if land.get("active"):
        condition += " / LANDING %s %s" % (clean(land.get("sha"), 80)[:7], clean(land.get("step"), 30) or "land")
    if room.get("other_rooms_incomplete"):
        condition += " / other rooms scan incomplete"
    essential = footer + (" / " + condition.strip(" / ") if condition else "")
    quotas = []
    for provider, reading in mapping(report.get("provider_status")).items():
        used = percent(mapping(reading).get("weekly_used"))
        if used is not None:
            quotas.append("%s week %s%% left" % (provider, 100 - used))
    quota = " / ".join(quotas)
    footer = essential + (" / " + quota if quota and display_width(essential + " / " + quota) <= width else "")
    add(footer, "metadata")
    if not menu:
        for text in footer_extra:
            add(text, "alert" if text == navigation.get("notice") else "metadata")
    final_hints = hints or footer_hints(navigation, width, managed, unicode, selected_member, True)
    if navigation.get("tab") == "debate" and not mapping(navigation.get("debate_view")).get("shown") and not menu:
        final_hints = footer_hints(dict(navigation, debate_empty=True), width, managed, unicode, selected_member, True)
    for text in final_hints[:help_rows]:
        add(text, "metadata")
    if menu:
        for text in footer_extra:
            add(text, "metadata")
    if bar:
        bar_y = len(lines) - 1
        hits += [{"y": bar_y, "x1": x1, "x2": x2, "action": ("spawn", name)} for x1, x2, name in bar[1]]
    items = [action(c) for c in judges] + ([action(primary)] if primary else []) + [action(c) for c in workers]
    items += [hit["action"] for hit in hits if hit["action"][0] in {"result", "debate"}]
    navigation.update(hits=hits, items=list(dict.fromkeys(items)), viewport=max(1, len(drawn)), offset=0,
        positions={item: i for i, item in enumerate(items)}, body_rows=len(children), room_id=room.get("id"),
        graph_selected=selected, surface="graph", primary=action(primary) if primary else None, bands=bands)
    limit = max(0, height - int(menu))
    if len(lines) > limit:
        # Tiny direct graph callers retain recovery text; ordinary navigation uses the tree fallback.
        tail = footer_extra if menu and footer_extra else lines[-2:]
        lines = lines[:max(0, limit - len(tail))] + tail[-limit:] if limit else []
        hits = [h for h in hits if h["y"] <= len(lines) - len(tail)]
        navigation["hits"] = hits
    text = "\n".join(lines[:limit])
    return text if unicode else text.translate({ord("·"): "/", ord("→"): ">", ord("×"): "x", ord("│"): "|"})


def summary_card(member, preview, width, room=None, unicode=True):
    """Eight evidence lanes, independent of the full retained-answer reader."""
    if not member:
        return ["No call selected / click a main or a call to see it here"], {}
    room = mapping(room)
    loaded = mapping(preview.get("report"))
    rows = listing(loaded.get("rows"))
    row = next((r for r in rows if member.get("task_id") and r.get("task_id") == member["task_id"]), next(iter(rows), {}))
    calls = listing(row.get("calls"))
    own_role = member.get("purpose") or member.get("role")
    candidates = [c for c in calls if c.get("answer") and (own_role == "reviewer" or c.get("kind") != "review")]
    own_call = next((c for c in reversed(candidates) if member.get("attempt") and c.get("attempt_id") == member["attempt"]),
                    next(iter(reversed(candidates)), {}))
    answer = str(own_call.get("answer") or "")
    lines = [member_identity(member) + " " + execution_tag(member, unicode)]
    if any(c.get("answer_truncated") is True for c in calls):
        lines.append("[partial answer] Retained excerpt / open the reader for source limits")
    transitions = listing(member.get("transitions"))
    recorded = ["%s %s" % (clock_stamp(t.get("ts")) or "unknown", clean(t.get("state"), 30))
                for t in map(mapping, transitions) if t.get("state")]
    if not recorded:
        if member.get("joined_at"):
            recorded.append((clock_stamp(member["joined_at"]) or "unknown") + " launched")
        if member.get("ended"):
            recorded.append((clock_stamp(member["ended"]) or "unknown") + " " + execution_tag(member, unicode))
    lines.append("Transitions: " + ((" " + glyph("right", unicode) + " ").join(recorded) or "not recorded"))
    verification = mapping(row.get("verification"))
    code = verification.get("exit")
    if type(code) is not int:
        code = next((c["verify_exit"] for c in reversed(calls) if type(c.get("verify_exit")) is int), None)
    verify = "command exit %s" % code if type(code) is int else "checks not run" if verification.get("status") == "not_run" else "checks unknown"
    from terminal_panel import _review_verdict
    review, source = None, None
    for message in reversed(listing(room.get("messages"))):
        if message.get("id") != "judgment-" + member["participant"]:
            continue
        text = str(message.get("text") or "")
        review = _review_verdict(text)
        if review is None:
            review = "pass" if text.startswith("Review passed for task ") else "fail" if text.startswith("Review verdict FAIL for task ") else None
        if review:
            source = "judgment mail"
            break
    if not review:
        review_call = next((c for c in reversed(calls) if c.get("kind") == "review" and c.get("answer")), None)
        review_text = review_call.get("answer") if review_call else answer if member.get("role") == "reviewer" or member.get("purpose") == "reviewer" else ""
        review = _review_verdict(review_text)
        source = "parsed answer" if review else None
    decision = {"accepted": "admitted", "completed": "completed (admission not recorded)", "failed": "rejected"}.get(row.get("outcome"), "pending")
    lines.append("Verify: %s / Review: %s / Main decision: %s" % (
        verify, (review + " (" + source + ")") if review else "not recorded", decision))
    lines.append("Next: " + ("decision recorded; check landing evidence" if decision == "admitted" else
                              "check findings and the patch, then admit or reject" if rows else "wait for recorded result, then check evidence"))
    role = member.get("purpose") or member.get("role")
    headings = {"worker": ("What changed",), "builder": ("What changed",), "advisor": ("Answer", "Recommendation"),
                "researcher": ("Answer", "Recommendation"), "reviewer": ("Findings",), "critic": ("Findings",),
                "council": ("Recommendation", "Disagreement")}.get(role, ())

    def excerpt(names):
        from panel_debate import HEADING
        current, found = None, []
        for line in answer.splitlines():
            heading = HEADING.match(line)
            inline = re.match(r"^\s*(%s):\s*(.*)$" % "|".join(re.escape(n) for n in names), line, re.I) if names else None
            if heading:
                current = (heading[1] or heading[2]).strip()
            elif inline:
                current = inline[1]
                found.append(inline[2])
            elif current and current.lower() in {n.lower() for n in names} and line.strip():
                found.append(line)
        return readable(" ".join(found), " ")

    headline = excerpt(headings) if headings else ""
    if not headline:
        headline = next((readable(p, " ") for p in answer.split("\n\n") if readable(p, " ")), "")
        label = "First paragraph (excerpt)"
    else:
        label = " / ".join(headings) + " (excerpt)"
    if member.get("role") == "main":
        status = declared_status(room, member["participant"], width)
        headline = " / ".join(status) or "No latest statement recorded"
        label = "Latest statement"
    if member.get("_native"):
        headline, label = "Its answer stays inside the main's chat; no retained result", "Built-in advisor"
    if preview.get("error"):
        headline, label = readable(preview["error"], " "), "Result unavailable"
    elif not rows and member.get("role") != "main" and not member.get("_native"):
        headline, label = "No result recorded yet" if preview.get("report") is not None else ("Reading recorded result…" if unicode else "Reading recorded result..."), "Result"
    lines.append(label + ": " + (headline or "No answer recorded"))
    limits = excerpt(("Remaining uncertainty", "Limits", "Limitations"))
    lines.append("Author-reported limits: " + (limits or "not recorded"))
    changes = next((mapping(c.get("changes")) for c in reversed(calls) if c.get("changes")), {})
    change = "unavailable" if changes.get("issue") else "%s files +%s -%s" % (
        changes.get("count"), changes.get("added", "?"), changes.get("removed", "?")) if "count" in changes else "not recorded"
    source_path = clean(own_call.get("artifact"), 160)
    lines.append("Changes: " + change + (" / source: " + source_path if source_path else ""))
    summary = readable(row.get("summary"), " ")
    if summary:
        lines.append("Owner summary: " + summary)
    return [clipped(line, width) for line in lines], {}


def dock_body_rows(height):
    """One body height for every bottom tab, so Detail, Plan and Debate do not resize the board."""
    return 12 if height >= 56 else 8 if height >= 40 else 5 if height >= 28 else 2


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
        primary = next((m for m in mains if main_attempt and main_attempt in (m["participant"], m.get("attempt"))),
                       next(iter(mains), None))
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
    # A call the board still draws, carried or owed from an earlier room, is not past.
    shown = {action(c) for c in past} | {action(m) for m in members if m.get("role") != "main" and classify(m) != "past"}
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

    from panel_debate import signature, tab_body as debate_body
    from panel_messages import tab_body as messages_body
    top = 1
    from panel_view import attention_items
    attention = attention_items(report, unicode) if attention is None else attention
    active = navigation.get("tab") if navigation.get("tab") in dict(TABS) else "detail"
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
    offset_key = (active, action(previewed) if active == "detail" and previewed else None)
    offsets = navigation.setdefault("dock_offsets", {})
    previous_key = navigation.get("dock_offset_key")
    if previous_key is not None and previous_key != offset_key:
        offsets[previous_key] = scrolled.get("detail", 0)
        scrolled["detail"] = offsets.get(offset_key, 0)
    navigation["dock_offset_key"] = offset_key
    body, targets = summary_card(previewed, preview, width - 4, room, unicode) if previewed else (
        ["No call selected · click a main or a call to see it here"], {})
    if previewed and previewed.get("role") == "main":
        native = next((c for c in everyone.get(previewed["participant"], []) if c.get("_native")), None)
        if native:
            note = "Built-in advisor: " + ("asked %s · running" % native["_native"] if native["state"] != "done"
                                           else "last answered %s" % (native.get("_finished") or native["_native"]))
            changes = next((i for i, line in enumerate(body) if line.startswith("Changes:")), len(body))
            body.insert(changes, clipped(note, width - 4))
    strip_only = rows_high < 3
    capacity = max(1, rows_high - 2)
    seen = navigation.setdefault("debate_seen", {})
    shown_main = primary["participant"] if primary else None
    digest = signature(report, shown_main)
    if active == "debate":
        seen[shown_main] = digest
    marks = {"messages": " %s" % unread_total, "past": " %s" % len(past), "needs": " %s" % len(attention)}
    tabs = list(TABS)
    navigation["visible_tabs"] = [key for key, unused in tabs]
    abbreviations = {"detail": "Dtl", "plan": "Plan", "debate": "Dbt", "messages": "Msg", "past": "Past", "needs": "Attn"}
    for short in (0, 1, 2):
        gap = " " if short == 2 else "  "
        tiny = {"detail": "D", "plan": "P", "debate": "B", "messages": "M", "past": "H", "needs": "A"}
        texts = [(("[%s]") if key == active else "%s") % (
            (name if short == 0 else abbreviations[key] if short == 1 else tiny[key]) +
            (marks.get(key, "") if short == 0 else " %s" % unread_total if key == "messages" and unread_total else
             " %s" % len(attention) if key == "needs" else " %s" % len(past) if key == "past" else ""))
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
    style = "border"
    colors, light = color_count(), light_theme()
    lines.append(paint(edge, style, color=color, colors=colors, light=light) if span is None else
                 paint(edge[:span[0]], style, color=color, colors=colors, light=light) +
                 paint(edge[span[0]:span[1]], "text", color=color, colors=colors, light=light, bold=True) +
                 paint(edge[span[1]:], style, color=color, colors=colors, light=light))
    # No room for a body: the strip alone keeps every tab one click away.
    if strip_only:
        navigation.pop("box", None)
    else:
        first, plain = 0, set()
        if active == "detail":
            reader_target = navigation.get("reader_target") if navigation.get("reader_open") else None
            if reader_target and previewed and reader_target != action(previewed):
                verdict = next((line for line in body if line.startswith("Verify:")), "verdict not recorded")
                reader_id = clipped(clean(str(reader_target[1]), 80), min(24, max(8, (width - 4) // 4)))
                prefix = "reader: %s %s selected" % (reader_id, "≠" if unicode else "!=")
                next_step = "Next: check selected evidence"
                available = max(1, width - 4 - display_width(prefix + next_step) - 6)
                compact_verdict = verdict.split(" / Review: ", 1)[-1].split(" / Main decision: ", 1)[0]
                body = [prefix + " / " + clipped(compact_verdict, available) + " / " + next_step]
            first = min(scrolled.get("detail", 0), max(0, len(body) - capacity))
            scrolled_now["detail"] = first
            rows = [(text, action(previewed) if previewed else None, False) for text in body[first:first + capacity]]
            if len(body) > capacity and rows:
                rows[-1] = (clipped(rows[-1][0], max(1, width - 34)) + " / rows %s-%s/%s / wheel" % (
                    first + 1, min(len(body), first + capacity), len(body)), rows[-1][1], False)
            if first > 0 and len(body) > 1 and body[1].startswith("[partial answer]") and rows:
                rows = [(body[1], action(previewed) if previewed else None, False)] + rows[:max(0, capacity - 1)]
            plain = set(range(capacity))
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
            if active == "detail" and n == 0 and previewed and first == 0:
                identity = clipped(member_identity(previewed), width - 4)
                line = line.replace(identity, paint(identity, role_style(previewed.get("role"), previewed.get("purpose")),
                    color=color, colors=colors, light=light, bold=True), 1)
            elif active in {"messages", "debate", "past"}:
                from panel_view import MODEL_NAMES
                identities = [member_identity(m, False) for m in members]
                if active == "debate":
                    identities += [call_identity("council", model) for model in MODEL_NAMES]
                for identity in sorted(identities, key=display_width, reverse=True):
                    if identity in line:
                        style_key = role_style(identity.split(" ", 1)[0].lower())
                        line = line.replace(identity, paint(identity, style_key, color=color, colors=colors, light=light, bold=picked))
                if active == "past":
                    ended_call = next((call for call in past if action(call) == target), None)
                    if ended_call:
                        tag = execution_tag(ended_call, unicode)
                        line = line.replace(tag, paint(tag, call_tone(ended_call), color=color, colors=colors, light=light), 1)
            elif active == "needs" and len(row) > 3:
                item = next((item for item in attention if item["row_id"] == row[3]), None)
                if item:
                    line = line.replace(text, paint(text, item["style"], color=color, colors=colors, light=light), 1)
            lines.append(line)
            if target:
                # Body text without its own link is passive, so a click that selects text changes nothing.
                hits.append({"y": top + 1 + n, "x1": 1, "x2": width, "action": target,
                             **({"preview": True, "passive": n in plain} if active == "detail" else
                                dict({active: True}, **{"key": row[3]} if len(row) > 3 else {})
                                if active in {"past", "needs"} else {})})
        lines.extend([box_row("", width, unicode)] * (rows_high - 2 - capacity))
        lines.append(paint(box_edge("", width, unicode, "bottom"), style, color=color, colors=colors, light=light))
        navigation["box"] = {"y1": top, "y2": top + rows_high - 1, "pages": bool(previewed) or active == "plan"}
    navigation["band_offsets"] = scrolled_now
    offsets[offset_key] = scrolled_now.get("detail", 0)
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
        end = clock_stamp(call.get("ended")) if call.get("ended") else call.get("_finished") or "unknown"
        state = "timed_out" if call.get("exit") == 124 else call.get("state")
        if call.get("_decided"):
            code = call.get("exit")
            state = "done" if code == 0 else "failed" if type(code) is int else "unknown"
        acceptance = " [accepted]" if call.get("decision") == "accepted" else " [main decided]" if call.get("_decided") or call.get("decision") == "completed" else " [acceptance pending]"
        if call_ending(call) == "cancelled":
            acceptance = ""
        tag = execution_tag(dict(call, state=state), unicode)
        identity = member_identity(call)
        tails = [tag + acceptance + "  " + end, tag + acceptance + "  " + end[-5:], tag + acceptance]
        tail = next((value for value in tails if display_width(identity + value) + 2 <= width), tails[-1])
        if width < 60:
            identity = clean(call.get("title")) or identity
            tail = tag
            # A successful exit is redundant when it would hide the task; error tags keep their evidence.
            if display_width(identity + tail) + 2 > width and tag == status_tag("done", exit_code=0, unicode=unicode):
                tail = status_tag("done", unicode=unicode)
        available = max(1, width - display_width(tail) - 2)
        if available < 12 and width >= 60:
            tail = tag + (" [decided]" if call.get("_decided") else "")
            available = max(1, width - display_width(tail) - 2)
        left = clipped(identity, available)
        text = left + " " * max(2, width - display_width(left + tail)) + tail
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
    cancel_owner = navigation.get("cancel_owner") or {}
    cancelable = bool(navigation.get("cancel_allowed", True) and member and kind == "result"
                      and member.get("state") in {"starting", "working", "verifying"}
                      and cancel_owner.get("participant") and cancel_owner.get("attempt")
                      and member.get("parent") == cancel_owner.get("participant"))
    mail = bool(navigation.get("full_mail") and navigation.get("tab", "detail") == "detail"
                and navigation.get("tab_body_visible") and not navigation.get("detail"))
    mail_hint = ("f Open right" if managed else "f Full text") if mail else ""
    message_call = tabs and navigation.get("tab") == "messages" and navigation.get("message_has_call")
    if navigation.get("keys_help"):
        keys = ["? Hide", "Esc Back", "F5 Control" if managed else "", "k Capacity", "u Refresh",
                ("Enter Collapse" if navigation.get("messages_view", {}).get("open") is not None and navigation.get("messages_view", {}).get("open") == navigation.get("messages_view", {}).get("selected") else "Enter Expand") if tabs and navigation.get("tab") == "messages" else
                "Enter Open right" if managed and kind in {"result", "debate"} else "Enter Open",
                move + " Move", "Space Fold", "a Ask advisor",
                "w Worktree" if worktree else "", "c Cancel call" if cancelable else "",
                mail_hint if mail else "f Open right" if message_call or managed and navigation.get("full_result") else
                "f Full result" if navigation.get("full_result") else "",
                "Tab Next tab" if tabs else "", "g Graph", "t Tree",
                "n New main" if managed and not navigation.get("main_occupied") else "",
                "s Swap main" if managed and navigation.get("main_occupied") else "",
                "v " + ("Collapse" if navigation.get("expanded") else "Expand"), "b Attention",
                "Shift-drag Copy" if managed else "", "m Modes" if managed else "",
                toggle if managed else "q Quit", "F12 Keys" if managed else ""]
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
    if tab in {"past", "needs"} and tab + "_ids" in navigation and not navigation.get(tab + "_ids"):
        contextual = ["Tab Next tab"]
    if tab == "messages" and "message_ids" in navigation and not navigation.get("message_ids"):
        contextual = [arrows + " Filter", "Tab Next tab"]
    contextual = contextual or (["Esc Back to graph", "g Graph"] if navigation.get("group_tree") else []) + [
                  "Enter Open right" if right else "",
                  "f Open right" if managed and navigation.get("full_result") else "f Full result" if navigation.get("full_result") else "",
                  "Esc Close" if navigation.get("detail") or navigation.get("preview") and
                  navigation.get("tab_body_visible", True) else "",
                  "Enter Chat" if kind == "chat" else "Enter Fold" if kind == "group" else "" if right else "Enter Show",
                  "a Ask advisor" if kind == "chat" else "w Worktree" if worktree else "",
                  "Tab Next tab" if tabs and not navigation.get("detail") else "",
                  "Space Fold" if kind == "group" or kind == "chat" and not graph else ""]
    if navigation.get("cancel_pending"):
        contextual.insert(0, "Confirm in tmux prompt")
    elif cancelable:
        contextual.insert(0, "c Cancel call")
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

    if managed:
        base_reserved = [toggle, "F12 Keys"] if width >= 64 else ["F9", "F12 Keys"]
        swap_reserved = ["F9 Swap", "F12 Keys"]
        mode_reserved = ([toggle, "m Modes", "F12 Keys"] if width >= 64 else
                         ["F9 Swap", "m Modes", "F12 Keys"])
        reserved = base_reserved
        if width < 64 and pack(swap_reserved) == pack(base_reserved):
            reserved = swap_reserved
        primary = lambda hints: [hint for hint in hints if hint.startswith(("Enter ", "Esc ", "f "))]
        if (primary(pack(mode_reserved)) == primary(pack(base_reserved))
                and display_width("  ".join(mode_reserved)) <= width):
            reserved = mode_reserved
    else:
        reserved = ["q Quit", "? Keys"]
    packed = pack(reserved)
    tail = "  ".join(reserved)
    return ["  ".join(packed + reserved)] if display_width(tail) <= width else [clipped(tail, width)]
