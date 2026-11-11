"""Normalized per-session status: a thin projection of the panel report (`oms panel --sessions`).

Nothing here reads a new record or calls a provider. Rows come from room_view.nodes() (room members) and
panel_view.worker_rows() (attempts and delegation markers outside the room); the raw OMS state is mapped
through STATE_MAP, the one table. Unknown stays unknown: a raw state with no row maps to unknown/unknown.
"""
import calendar
import datetime
import re
import time

SCHEMA = 1
STATES = ("working", "waiting", "blocked", "done", "failed", "unknown")
WAITING_FOR = ("input", "approval", "review", "owner", "none", "unknown")

# raw OMS state -> (state, waiting_for). Every raw state OMS can produce has a row; the test derives that set
# from agent-events ALL_STATES, call_ending LIVE_STATES/OPEN_STATES and panel_view TERMINAL/ATTENTION_STATES.
#
# EXTENSION POINT (not implemented): PROVIDER_NATIVE_STATUS. Provider-native inputs such as
# `claude agents --json` would be read by a separate, opt-in reader and merged here per session id, never
# replacing the OMS record. This module calls no provider CLI and reads no provider file.
STATE_MAP = {
    # waiting on scheduling or the next prompt, not on a person's decision
    "queued": ("waiting", "none"),
    "idle": ("waiting", "none"),
    # running or driven by the person
    "starting": ("working", "none"),
    "working": ("working", "none"),
    "running": ("working", "none"),
    "verifying": ("working", "none"),
    "live marker": ("working", "none"),
    "manual": ("working", "none"),
    # waiting on a person
    "waiting_input": ("waiting", "input"),
    "waiting_approval": ("waiting", "approval"),
    "review": ("waiting", "review"),
    "blocked": ("blocked", "owner"),
    # finished
    "done": ("done", "none"),
    "exited": ("done", "none"),
    "cancelled": ("done", "none"),
    "failed": ("failed", "none"),
    "timed_out": ("failed", "none"),
    "abandoned": ("failed", "none"),
    # evidence does not say
    "quiet": ("unknown", "unknown"),
    "presence unknown": ("unknown", "unknown"),
    "orphaned": ("unknown", "unknown"),
}
ENDED = {"done", "exited", "cancelled", "failed", "timed_out", "abandoned"}


def normalize(raw):
    return STATE_MAP.get(raw, ("unknown", "unknown")) if isinstance(raw, str) else ("unknown", "unknown")


def _iso(value):
    if isinstance(value, str) and value:
        return value
    if type(value) in (int, float) and 0 < value < 4e9:
        return datetime.datetime.fromtimestamp(value, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return None


def _staleness(report):
    """(all_stale, attempts_stale, reason): held by the board, or a source the dashboard could not read."""
    collection = report.get("collection") if isinstance(report.get("collection"), dict) else {}
    sources = collection.get("sources") if isinstance(collection.get("sources"), dict) else {}
    bad = sorted(name for name, value in sources.items() if value != "ok")
    held = report.get("capacity_stale") is True
    room = report.get("room") if isinstance(report.get("room"), dict) else {}
    failed = collection.get("ok") is not True and not sources
    reasons = (["held previous read"] if held else []) + (["refresh failed"] if failed else []) + (
        ["unreadable: " + ", ".join(bad)] if bad else []) + (["room unreadable"] if room.get("error") else [])
    return held or failed or bool(room.get("error")), "attempts" in bad, "; ".join(reasons) or None


def project(report):
    from panel_view import worker_rows
    from room_view import main_state, nodes
    all_stale, attempts_stale, reason = _staleness(report)
    attempts = report.get("attempts") if isinstance(report.get("attempts"), dict) else {}
    updated = {row.get("attempt_id"): row.get("updated_at") for key in ("active_recent", "recent")
               for row in attempts.get(key) or [] if isinstance(row, dict)}
    sessions, seen = [], set()

    def add(ident, attempt, participant, provider, role, raw, since, basis, source):
        state, waiting = normalize(raw)
        sessions.append({"id": ident, "attempt": attempt, "participant": participant,
                         "provider": provider or "unknown", "role": role or "unknown", "state": state,
                         "waiting_for": waiting, "raw_state": raw, "since": since, "since_basis": basis if since else None,
                         "source": source, "stale": bool(all_stale or (attempt and attempts_stale))})

    for member in nodes(report):
        raw = main_state(member)
        attempt, participant = member.get("attempt"), member.get("participant")
        since, basis = None, None
        activity = member.get("panel_activity") if isinstance(member.get("panel_activity"), dict) else {}
        native = member.get("role") == "main" and raw != member.get("state") and raw != "exited"
        if native and _iso(activity.get("since")):
            since, basis = _iso(activity["since"]), "native activity"
        elif raw in ENDED:
            since = _iso(member.get("ended"))
            basis = "ended"
            if since is None and attempt:
                since, basis = _iso(updated.get(attempt)), "last event"
        add(attempt or participant, attempt, participant, member.get("provider"), member.get("role"), raw,
            since, basis, "native activity" if native else "attempt" if attempt else "room")
        seen.add(attempt)
    for row in worker_rows(report):
        attempt = row.get("attempt")
        if attempt and attempt in seen:
            continue
        raw = row.get("state")
        if attempt:
            since, basis = (_iso(updated.get(attempt)), "last event") if raw in ENDED else (None, None)
            add(attempt, attempt, None, row.get("provider"), row.get("role"), raw, since, basis, row.get("source"))
        else:
            add("marker:" + str(row.get("location") or row.get("task")), None, None, row.get("provider"),
                row.get("role"), raw, None, None, row.get("source") or "marker")
    return {"schema": SCHEMA, "kind": "oms-sessions", "stale": bool(all_stale or attempts_stale),
            "stale_reason": reason, "sessions": sessions}


ADAPTER_PROVIDER = re.compile(r"oms-agent-adapter-[a-z0-9][a-z0-9._-]{0,63}")
AGE_UNITS = {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}


def valid_providers():
    from panel_routing import MAIN_OWNERS
    return sorted({*MAIN_OWNERS, "agy", "unknown"})


def valid_roles():
    from panel_routing import KINDS
    from room_view import ROLES
    return sorted({*ROLES, *KINDS, "unknown"})


def check_filters(state=None, provider=None, role=None, min_age=None):
    """Refuse an unknown filter value with the valid list; returns (state, provider, role, min_age_seconds).

    A filter that names nothing recorded is still valid: it matches no row, it is not a typo."""
    if state is not None and state not in STATES:
        raise ValueError("unknown --state %r; valid: %s" % (state, ", ".join(STATES)))
    if provider is not None and provider not in valid_providers() and not ADAPTER_PROVIDER.fullmatch(provider):
        raise ValueError("unknown --provider %r; valid: %s, oms-agent-adapter-ID" % (provider, ", ".join(valid_providers())))
    if role is not None and role not in valid_roles():
        raise ValueError("unknown --role %r; valid: %s" % (role, ", ".join(valid_roles())))
    seconds = None
    if min_age is not None:
        match = re.fullmatch(r"([0-9]{1,9})(s|m|h|d|w)", min_age)
        if not match or int(match.group(1)) < 1:
            raise ValueError("invalid --min-age %r; use a positive whole duration such as 30m, 2h or 7d" % min_age)
        seconds = int(match.group(1)) * AGE_UNITS[match.group(2)]
    return state, provider, role, seconds


def narrow(report, state=None, provider=None, role=None, min_age=None, now=None):
    """The same projection with only the rows that match every given filter (arguments from check_filters).

    Age counts from `since`; a row with no `since` has no provable age, so --min-age never selects it. Stale and
    stale_reason stay on the report, so a held or unreadable read is not mistaken for an empty answer."""
    if state is None and provider is None and role is None and min_age is None:
        return report
    now = time.time() if now is None else now

    def old_enough(row):
        try:
            then = calendar.timegm(time.strptime(str(row["since"])[:19], "%Y-%m-%dT%H:%M:%S"))
        except (ValueError, TypeError):
            return False
        return now - then >= min_age

    rows = [row for row in report["sessions"]
            if (state is None or row["state"] == state) and (provider is None or row["provider"] == provider)
            and (role is None or row["role"] == role) and (min_age is None or old_enough(row))]
    return dict(report, sessions=rows)


def text(report):
    lines = []
    for row in report["sessions"]:
        lines.append("%s %s/%s %s waiting=%s raw=%s since=%s source=%s%s" % (
            row["id"], row["provider"], row["role"], row["state"], row["waiting_for"], row["raw_state"],
            row["since"] or "-", row["source"], " stale" if row["stale"] else ""))
    if report["stale"]:
        lines.append("stale: " + (report["stale_reason"] or "unknown"))
    return "\n".join(lines) if lines else "no sessions recorded"
