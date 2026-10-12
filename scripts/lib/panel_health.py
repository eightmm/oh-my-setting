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


def render(report, width, height, footer=None):
    from panel_view import clipped, LIVE_STATES
    from room_view import nodes
    members = nodes(report)
    providers = []
    for member in members:
        provider = "pi" if member.get("harness") == "pi" else member.get("provider")
        if provider in {"claude", "codex", "pi"} and provider not in providers:
            providers.append(provider)
    rows = ["OMS / Capacity and recovery"]
    stale = report.get("capacity_stale") is True
    if stale:
        rows.append("Stale readings; status unknown")
    readable = not stale and mapping(report.get("collection")).get("ok") is True
    for provider in providers:
        if provider == "pi":
            rows.append("-- [unknown] Pi: no native reading")
            continue
        row = mapping(mapping(report.get("provider_status")).get(provider))
        known = readable and row.get("source") not in {"cache stale", "unavailable"}
        context = percent(row.get("context_left")) if known else None
        weekly = percent(row.get("weekly_used")) if known else None
        warn = env_int("OMS_CTX_WARN_PCT", 15, minimum=1, maximum=90)
        urgent = min(env_int("OMS_CTX_URGENT_PCT", 8, minimum=0, maximum=89), warn)
        status = "unknown" if context is None else "full" if context <= 0 else "urgent" if context <= urgent else "warning" if context <= warn else "ok"
        rows.append(("%s main: context [%s]" % (provider.capitalize(), status) if context is None else
                     "%s%% left [%s] %s main" % (context, status, provider.capitalize())))
        rows.append("%s used [%s] %s weekly" % ("--" if weekly is None else "%s%%" % weekly,
                    "unknown" if weekly is None else "full" if weekly >= 100 else "warning" if weekly >= 90 else "ok", provider.capitalize()))
    mains = sum(m.get("role") == "main" and m.get("state") in
                LIVE_STATES | {"blocked", "waiting_input", "waiting_approval", "review"} for m in members)
    rows.append("Main: %s live" % mains)
    mains = mapping(report.get("dead_mains"))
    dead = mains.get("releasable") or []
    if dead:
        rows.append("Dead mains")
        for main in dead:
            rows.append("  %s / %s / %s / %s / %s" % tuple(clean(main.get(key), 160) for key in
                        ("attempt", "provider", "state", "age", "label")))
            rows.append("  oms panel --release-main " + clean(main.get("attempt"), 160))
    unknown = mains.get("unverifiable") or []
    if unknown:
        rows.append("Unverifiable mains")
        for main in unknown:
            rows.append("  %s / %s / %s / %s / %s [unverifiable]" % tuple(clean(main.get(key), 160) for key in
                        ("attempt", "provider", "state", "age", "label")))
            rows.append("  " + clean(main.get("reason"), 160))
    for item in project(report)["items"]:
        if item["kind"] in {"context", "quota"} or item["label"] == "Room mains":
            continue
        value = "--" if item["used"] is None else "%s/%s" % (item["used"], item["limit"])
        rows.append("%s [%s] %s" % (value, item["status"], item["label"]))
        if item["status"] != "ok":
            rows.append("  " + item["action"])
    footer = footer if footer is not None else "k / Esc board; u / Ctrl+L refresh"
    omitted = max(0, len(rows) - max(0, height - 2))
    if omitted:
        rows = rows[:max(0, height - 2)] + ["%d more; oms panel --health --json" % omitted]
    return "\n".join(clipped(line, width) for line in (rows + [footer])[:max(0, height)])
