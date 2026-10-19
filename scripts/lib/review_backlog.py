"""Main-owned results waiting for review or admission."""

from dashboard_projection import listing, mapping

THRESHOLD = 8


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
    awaiting = set(listing(report.get("awaiting_admission")))
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
                or finalized.get(task.get("id")) in DECIDED):
            continue
        owner = mains.get(task.get("claimed_by_participant") or task.get("claimed_by"))
        if owner:
            totals[owner] = totals.get(owner, 0) + 1
    return totals


def warning(count):
    return "%s results await review/admission; review first" % count


def project(report):
    """Capture the render-time warning threshold and counts once per snapshot."""
    try:
        totals = counts(report)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError):
        totals = {}
    return {"threshold": THRESHOLD, "counts": totals}


def warning_for(report, participant):
    projection = mapping(report.get("review_backlog"))
    total = mapping(projection.get("counts")).get(participant, 0)
    return warning(total) if total > projection.get("threshold", THRESHOLD) else None
