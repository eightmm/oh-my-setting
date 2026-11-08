"""Main-owned results waiting for review or admission."""

from dashboard_projection import listing, mapping

# One work-in-progress limit per main: live write workers plus results awaiting decision.
THRESHOLD = 4


def counts(report):
    """Return pending result counts by owning main participant."""
    from room_view import DECIDED, call_classifier, nodes

    members = nodes(report)
    mains = {key: member["participant"] for member in members if member.get("role") == "main"
             for key in (member.get("participant"), member.get("attempt")) if key}
    totals = {}
    finalized = mapping(report.get("finalized"))
    classify = call_classifier(report)
    counted_tasks = set()
    handled_tasks = set()
    awaiting = set(listing(report.get("awaiting_admission")))
    # Prefer arranged copies, then restore supersession for predecessors hidden by rollover.
    seen_participants = {m.get("participant") for m in members if isinstance(m, dict) and m.get("participant")}
    all_calls = [m for m in members if isinstance(m, dict)] + [
        c for c in (getattr(report, "memo", {}).get("calls") or [])
        if isinstance(c, dict) and c.get("participant") not in seen_participants]
    room = mapping(report.get("room"))
    depth = {old: -n for n, old in enumerate(listing(room.get("lineage")), 1)}
    origins = mapping(room.get("call_origins"))
    order = lambda m: tuple(origins.get(m.get("participant")) or (
        depth.get(m.get("_predecessor"), -1) if m.get("_predecessor") else 0, m.get("seq", 0)))
    superseded = {id(m) for m in all_calls if m.get("superseded")}
    latest = {}
    for member in sorted(all_calls, key=order):
        if member.get("role") != "main" and member.get("task_id"):
            key = (member.get("role"), member["task_id"])
            if key in latest:
                superseded.add(id(latest[key]))
            latest[key] = member
    for member in all_calls:
        owner = mains.get(member.get("parent"))
        state = member.get("state")
        if (owner and member.get("task_id") and member.get("role") == "worker"
                and member.get("access") != "read" and member.get("purpose") != "investigate"
                and id(member) not in superseded
                and (state == "cancelled" or state == "done"
                     and report.get("awaiting_admission") is not None
                     and member.get("participant") not in awaiting)):
            handled_tasks.add((owner, member["task_id"]))
    # Count actionable calls as the inbox does, including a review call whose plan
    # row has not advanced yet. A task review is the fallback when no call represents it.
    for member in members:
        state = member.get("state")
        if (member.get("role") == "main" or classify(member) == "past"
                or not (state == "review" and finalized.get(member.get("task_id")) not in DECIDED
                        or state == "done" and member.get("participant") in awaiting)):
            continue
        owner = mains.get(member.get("parent"))
        if owner:
            totals[owner] = totals.get(owner, 0) + 1
            if member.get("task_id"):
                counted_tasks.add(member["task_id"])
    for task in listing(mapping(report.get("plan")).get("tasks")):
        task = mapping(task)
        if (task.get("state") != "review" or task.get("id") in counted_tasks
                or finalized.get(task.get("id")) in DECIDED
                or task.get("panel_accept") or task.get("landing")):
            continue
        owner = mains.get(task.get("claimed_by_participant") or task.get("claimed_by"))
        if owner and (owner, task.get("id")) not in handled_tasks:
            totals[owner] = totals.get(owner, 0) + 1
    return totals


def running(report):
    """Return live write worker counts by owning main participant."""
    from room_view import call_classifier, nodes
    from call_ending import LIVE_STATES

    members = nodes(report)
    mains = {key: member["participant"] for member in members if member.get("role") == "main"
             for key in (member.get("participant"), member.get("attempt")) if key}
    classify = call_classifier(report)
    totals = {}
    for member in members:
        owner = mains.get(member.get("parent"))
        if (owner and member.get("role") != "main" and member.get("access") == "write"
                and member.get("state") in LIVE_STATES and classify(member) == "live"):
            totals[owner] = totals.get(owner, 0) + 1
    return totals


def warning(running_count, awaiting):
    return ("WIP limit %s reached: %s running, %s awaiting decision; review first"
            % (THRESHOLD, running_count, awaiting))


def project(report):
    """Capture the render-time limit, running and awaiting counts once per snapshot."""
    try:
        totals, live = counts(report), running(report)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError):
        totals, live = {}, {}
    return {"threshold": THRESHOLD, "counts": totals, "running": live}


def warning_for(report, participant):
    projection = mapping(report.get("review_backlog"))
    awaiting = mapping(projection.get("counts")).get(participant, 0)
    live = mapping(projection.get("running")).get(participant, 0)
    limit = projection.get("threshold", THRESHOLD)
    return warning(live, awaiting) if live + awaiting >= limit else None
