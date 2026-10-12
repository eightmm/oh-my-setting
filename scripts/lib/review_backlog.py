"""Main-owned results waiting for review or admission."""

from dashboard_projection import listing, mapping
from panel_routing import policy

DEFAULT_THRESHOLD = 8


def counts(report):
    """Return pending result counts by owning main participant."""
    from room_view import nodes

    members = nodes(report)
    mains = {key: member["participant"] for member in members if member.get("role") == "main"
             for key in (member.get("participant"), member.get("attempt")) if key}
    totals = {}
    for task in listing(mapping(report.get("plan")).get("tasks")):
        task = mapping(task)
        if task.get("state") != "review":
            continue
        owner = mains.get(task.get("claimed_by_participant") or task.get("claimed_by"))
        if owner:
            totals[owner] = totals.get(owner, 0) + 1
    awaiting = set(listing(report.get("awaiting_admission")))
    for member in members:
        owner = mains.get(member.get("parent"))
        if (member.get("role") == "worker" and member.get("access") == "write"
                and owner and member.get("state") == "done"
                and member.get("participant") in awaiting):
            totals[owner] = totals.get(owner, 0) + 1
    return totals


def warning(count):
    return "%s results await review/admission; review first" % count


def project(report):
    """Capture the render-time warning threshold and counts once per snapshot."""
    try:
        threshold = policy().get("review_backlog_warn", DEFAULT_THRESHOLD)
    except (OSError, ValueError, TypeError, AttributeError):
        threshold = DEFAULT_THRESHOLD
    try:
        totals = counts(report)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError):
        totals = {}
    return {"threshold": threshold, "counts": totals}


def warning_for(report, participant):
    projection = mapping(report.get("review_backlog"))
    total = mapping(projection.get("counts")).get(participant, 0)
    return warning(total) if total > projection.get("threshold", DEFAULT_THRESHOLD) else None


def needs_review_hint(report, participant):
    return warning_for(report, participant) is not None
