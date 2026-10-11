"""Read-only pressure projection; reported limits never authorize native session changes."""

from dashboard_projection import clean, mapping
from panel_metrics import percent
from hook_state import env_int
import room


def project(report):
    items = []

    def add(kind, label, used, limit, action, unknown=False, warning_at=None, urgent_at=None):
        unknown = (unknown or report.get("capacity_stale") is True or type(used) is not int or used < 0
                   or type(limit) is not int or limit <= 0)
        status = ("unknown" if unknown else "full" if used >= limit else
                  "urgent" if urgent_at is not None and used >= urgent_at else
                  "warning" if used >= (warning_at if warning_at is not None else limit * .9) else "ok")
        items.append({"kind": kind, "label": label, "used": None if unknown else used,
                      "limit": limit, "status": status, "action": action})

    for provider in ("claude", "codex"):
        row = mapping(mapping(report.get("provider_status")).get(provider))
        context, weekly = percent(row.get("context_left")), percent(row.get("weekly_used"))
        owner = clean(row.get("participant"), 80)
        title = provider.capitalize() + (" / " + owner if owner else "")
        readable = mapping(report.get("collection")).get("ok") is True and row.get("source") not in {
            "cache stale", "unavailable"}
        warn = env_int("OMS_CTX_WARN_PCT", 15, minimum=1, maximum=90)
        urgent = min(env_int("OMS_CTX_URGENT_PCT", 8, minimum=0, maximum=89), warn)
        add("context", title + " context used", None if context is None else 100 - context, 100,
            "Use native compaction or an explicit handoff; clearing the board does not free model context.",
            context is None or not readable, warning_at=100 - warn, urgent_at=100 - urgent)
        add("quota", provider.capitalize() + " reported weekly use", weekly, 100,
            "Check the native quota/reset; OMS keeps the selected model and provider.", weekly is None or not readable)
    shared = mapping(report.get("room"))
    members = shared.get("participants")
    if isinstance(members, list) and not shared.get("error"):
        if shared.get("closed") is True:
            add("room", "Room closed", 1, 1, "Select a fresh room for new work; retained room evidence stays readable.")
        for called, label, limit, reserve in ((False, "Room mains", room.MAX_PARTICIPANTS, room.MAIN_RESERVE),
                                              (True, "Room calls", room.MAX_CALLS, room.CALL_RESERVE)):
            add("room", label, sum(bool(mapping(member).get("parent")) == called for member in members), limit,
                "New mains use a fresh bounded room near capacity; existing sessions and room evidence stay intact.",
                warning_at=limit - reserve + 1)
        messages = shared.get("message_count")
        add("room", "Room messages", messages if type(messages) is int else None, room.MAX_MESSAGES,
            "Select a fresh room for new work; acknowledgement does not delete retained messages.", type(messages) is not int,
            warning_at=room.MAX_MESSAGES - room.MESSAGE_RESERVE + 1)
    for kind, row in mapping(report.get("private_capacity")).items():
        row = mapping(row)
        if row.get("error"):
            add("slots", kind.capitalize() + " slots", None, None, row["error"], True)
            continue
        add("slots", kind.capitalize() + " slots", row.get("used"), row.get("limit"),
            "Completed/gone records are archived automatically at capacity; live/unknown items remain owned.")
        add("history", kind.capitalize() + " history", row.get("archived"), row.get("archive_limit"),
            "History retains logs and recipes. Review it before explicitly forgetting individual records.")
    if mapping(report.get("collection")).get("ok") is not True:
        items.append({"kind": "collection", "label": "Status collection", "used": None, "limit": None,
                      "status": "unknown", "action": "Refresh status; the retained frame is not current execution proof."})
    level = next((state for state in ("full", "urgent", "warning", "unknown") if any(i["status"] == state for i in items)), "ok")
    return {"schema": 1, "level": level, "items": items}


def alerts(report):
    pressured = [item for item in project(report)["items"] if item["status"] in {"full", "urgent", "warning"}]
    return ["! Capacity: " + "; ".join("%s %s/%s" % (item["label"], item["used"], item["limit"])
            for item in pressured)] if pressured else []


def render(report, width, height):
    from panel_view import clipped
    rows = ["OMS / Capacity and recovery" + (" / stale" if report.get("capacity_stale") is True else "")]
    dead = report.get("dead_mains") or []
    if dead:
        rows.append("Dead mains")
        for main in dead:
            rows.append("  %s / %s / %s / %s / %s" % tuple(clean(main.get(key), 160) for key in
                        ("attempt", "provider", "state", "age", "label")))
            rows.append("  oms panel --release-main " + clean(main.get("attempt"), 160))
    data = project(report)
    for item in data["items"]:
        value = "--" if item["used"] is None else "%s/%s" % (item["used"], item["limit"])
        rows.append("%s: %s [%s]" % (item["label"], value, item["status"]))
        if item["status"] != "ok":
            rows.append("  " + item["action"])
    footer = "k / Esc board; u / Ctrl+L refresh display"
    omitted = max(0, len(rows) - max(0, height - 2))
    if omitted:
        rows = rows[:max(0, height - 2)] + ["%d more lines; oms panel --health --json has the complete report" % omitted]
    return "\n".join(clipped(line, width) for line in (rows + [footer])[:max(0, height)])
