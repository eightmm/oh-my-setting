"""One normalized, read-only usage snapshot per provider, and the board's provider strip.

Nothing here reads a new file or calls a provider. The snapshot is a projection of the board's own
`provider_status` rows (panel_metrics.collect: the Claude HUD cache, the Codex session log, the OMS
artifact index for Antigravity) and of the room's own members for the live seat count. A value a provider
did not report is `{"unreported": reason}`, never a zero or an estimate; quota is never derived from tokens.
`oms panel --usage [--json]` prints the snapshot; the board draws `strip()`.
"""

import time

from dashboard_projection import clean, display_width, mapping, number
from panel_metrics import READING_FRESH_SECONDS, five_hour, percent, quota_bar

SCHEMA = 1
# name, long name, strip name, one-letter name, palette hue. The panel has no per-provider hue of its own, so Claude
# takes the warm main colour, Codex the counsel blue and Antigravity the lavender judge colour.
PROVIDERS = (("claude", "Claude", "Claude", "C", "main"),
             ("codex", "Codex", "Codex", "X", "counsel"),
             ("antigravity", "Antigravity", "agy", "A", "judge"))
# The health screen's own "warning" band for a weekly or 5-hour window: 10% left or less.
CRITICAL_USED = 90
# cached() drops the numbers of a HUD record older than the freshness limit; the snapshot says so instead of guessing.
STALE_HUD = "HUD cache over %d min old; numbers not shown" % (READING_FRESH_SECONDS // 60)


def unreported(reason):
    return {"unreported": reason}


def window_label(minutes):
    """300 minutes is the 5-hour window; any other window length is named by its minutes, never guessed."""
    return "5h" if minutes == 300 else "%dm" % minutes


def _remaining(seconds):
    if seconds < 60:
        return "<1m"
    minutes = int(seconds // 60)
    if minutes < 60:
        return "%dm" % minutes
    hours = minutes // 60
    if hours < 48:
        return "%dh%02dm" % (hours, minutes % 60)
    return "%dd%dh" % divmod(hours, 24)


def _age(observed, now):
    stamp = number(observed)
    return None if stamp is None or stamp > now else now - stamp


def _window(name, used, resets_at, resets_text, observed, source, now, held, warned):
    age = _age(observed, now)
    reset = number(resets_at)
    stale = held or (age is not None and age > READING_FRESH_SECONDS)
    entry = {"name": name, "used": used, "source": source, "stale": stale,
             "resets_at": reset if reset is not None else unreported("the provider reported no reset time"),
             "resets_in": (resets_text or unreported("no usable reset time reported")) if resets_text is not None else (
                 _remaining(reset - now) if reset is not None and reset > now else
                 unreported("no reset time reported")),
             "age_seconds": age if age is not None else unreported("no observation time recorded for this reading")}
    # Cues come from the thresholds the panel already has, and only a fresh reading earns one.
    entry["level"] = (None if stale else "critical" if used >= CRITICAL_USED else "warn" if warned else "ok")
    return entry


def _seats(report):
    if mapping(report.get("collection")).get("ok") is not True or mapping(report.get("room")).get("error"):
        return None
    from panel_view import WORKER_ROLES
    from room_view import main_state, nodes
    from session_status import normalize
    seats = {}
    for member in nodes(report):
        role = member.get("role")
        if role != "main" and role not in WORKER_ROLES:
            continue
        if normalize(main_state(member))[0] in {"working", "waiting"}:
            row = seats.setdefault(member.get("provider"), {"main": 0, "worker": 0})
            row["main" if role == "main" else "worker"] += 1
    return seats


def snapshot(report, now=None):
    """Schema 1 snapshot: {"schema", "now", "fresh_seconds", "held", "providers": {name: {...}}}."""
    from panel_health import quota_warnings
    now = time.time() if now is None else now
    status = mapping(report.get("provider_status"))
    held = report.get("capacity_stale") is True
    collected = mapping(report.get("collection")).get("ok") is True
    warned = {ident for ident, unused in quota_warnings(report, now)}
    seats = _seats(report)
    providers = {}
    for key, title, unused_short, unused_letter, unused_token in PROVIDERS:
        row = mapping(status.get(key))
        source = row.get("source")
        out = {"name": title, "windows": [], "windows_unreported": {}}
        providers[key] = out
        gone = "status collection unavailable" if not collected else None

        # Account label: only Codex has OMS-registered accounts today.
        label = clean(row.get("account_label"), 64)
        out["account"] = label if label else unreported(
            {"claude": "OMS has no Claude account registry",
             "codex": "no OMS-registered Codex account matches the current Codex home",
             "antigravity": "OMS has no Antigravity account registry"}[key])

        # Quota windows.
        if key == "antigravity":
            reason = "agy reports no quota window locally"
            out["windows_unreported"] = {"5h": reason, "weekly": reason}
        else:
            origin = {"HUD": "HUD cache", "HUD/TUI": "HUD cache", "session log": "Codex session log",
                      "TUI": "pane footer"}.get(source)
            if origin is None and source not in {"unavailable", "cache stale"} and (source or any(
                    row.get(field) is not None for field in ("weekly_used", "five_hour_used", "context_left"))):
                origin = clean(source, 40) or "provider status"
            observed = row.get("observed_at") if source in {"HUD", "HUD/TUI", "session log"} else None
            if key == "claude":
                window = five_hour(row, now) if collected and origin else None
                if window is not None:
                    out["windows"].append(_window("5h", 100 - window[0], row.get("five_hour_resets_at"), window[1],
                                                  observed, origin, now, held, ("quota", key, "5h") in warned))
                else:
                    out["windows_unreported"]["5h"] = (
                        gone or (STALE_HUD
                                 if source == "cache stale" else
                                 "pane footer carries no 5h window" if source == "TUI" else
                                 "no HUD cache: no Claude main selected, or it recorded none" if not origin else
                                 "HUD cache carries no 5h window, or it already reset"))
            else:
                # Codex's session log records its windows by length: 300 minutes is the 5-hour one, any other
                # non-weekly length is shown under its own minutes rather than guessed at.
                shorts = [w for w in row.get("short_windows") or () if isinstance(w, dict)] if (
                    collected and origin == "Codex session log") else []
                for short in shorts:
                    minutes = short.get("minutes")
                    used = percent(short.get("used"))
                    if used is None or not isinstance(minutes, int) or isinstance(minutes, bool) or minutes <= 0:
                        continue
                    out["windows"].append(_window(window_label(minutes), used, short.get("resets_at"), None, observed,
                                                  origin, now, held, False))
                if not any(w["name"] == "5h" for w in out["windows"]):
                    out["windows_unreported"]["5h"] = gone or "no 5-hour rate limit in the newest Codex session log"
            weekly = percent(row.get("weekly_used")) if collected and origin else None
            if weekly is not None:
                out["windows"].append(_window("weekly", weekly, row.get("weekly_resets_at"), None, observed, origin,
                                              now, held, ("quota", key, "week") in warned))
            else:
                out["windows_unreported"]["weekly"] = (
                    gone or (STALE_HUD
                             if source == "cache stale" else
                             "no HUD cache: no Claude main selected, or it recorded none" if key == "claude" and not origin else
                             "no weekly rate limit in the newest Codex session log" if key == "codex" else
                             "HUD cache carries no weekly window, or it already reset"))
        # A held or stale source with nothing to show still says how old it is.
        age = _age(row.get("observed_at"), now) if source == "cache stale" else None
        out["reading"] = {"stale": bool(held or source == "cache stale" or any(w["stale"] for w in out["windows"])),
                          "age_seconds": age if age is not None else (
                              next((w["age_seconds"] for w in out["windows"]), unreported("no reading")))}

        # Context, tokens, cache share.
        context = percent(row.get("context_left")) if collected and key != "antigravity" else None
        out["context_used"] = (100 - context if context is not None else unreported(
            "agy reports no context locally" if key == "antigravity" else
            gone or "no context reading for the selected main"))
        no_total = "no per-provider token total is recorded for today"
        out["tokens"] = {"today": unreported(no_total)}
        if key == "antigravity" and source == "OMS artifacts":
            out["tokens"]["recent_7d"] = {
                "calls": row.get("calls"), "input": row.get("input_tokens"), "cache_read": row.get("cache_read_tokens"),
                "output": row.get("output_tokens"), "source": "OMS artifacts (calls OMS delegated to agy only)"}
        else:
            out["tokens"]["recent_7d"] = unreported(
                "no OMS artifact usage record for antigravity" if key == "antigravity" else
                "no OMS-read record sums this provider's tokens")
        share = percent(row.get("cache_read_share")) if collected and key == "claude" else None
        out["cache_read_share"] = ({"percent": share, "scope": "last request", "source": "HUD cache"} if share is not None
                                   else unreported({"claude": "the status line recorded no current_usage counts",
                                                    "codex": "Codex session logs are not read for cache counts",
                                                    "antigravity": "agy cache and input counters are not comparable "
                                                    "as a share; see tokens"}[key]))
        live = None if seats is None else mapping(seats.get(key))
        out["seats"] = (unreported("status collection unavailable or room unreadable") if live is None else
                        {"live": live.get("main", 0) + live.get("worker", 0),
                         "main": live.get("main", 0), "workers": live.get("worker", 0)})
        fresh = [w for w in out["windows"] if not w["stale"]]
        pool = fresh or out["windows"]
        out["tightest"] = max(pool, key=lambda w: w["used"])["name"] if pool else None
    return {"schema": SCHEMA, "now": now, "fresh_seconds": READING_FRESH_SECONDS, "held": held, "providers": providers}


def _tightest(provider):
    return next((w for w in provider["windows"] if w["name"] == provider["tightest"]), None)


def _age_text(provider):
    age = provider["reading"]["age_seconds"]
    return _remaining(age) if isinstance(age, (int, float)) else ""


def _seat_count(provider):
    seats = provider["seats"]
    return "?" if "unreported" in seats else str(seats["live"])


def _segment(provider, key, mode, unicode, now):
    """Pieces [(text, hue)] of one provider at one detail level."""
    short, letter, hue = next(entry[2:] for entry in PROVIDERS if entry[0] == key)
    tight = _tightest(provider)
    dash = "—" if unicode else "--"
    terse = mode in {"short_bars", "short", "minimal"}
    pieces = [(letter if terse else short, hue)]
    if tight is None:
        pieces.append((" " + dash, "dim"))
        if provider["reading"]["stale"]:
            pieces.append(("~" if terse else " stale " + _age_text(provider), "dim"))
    else:
        tone = {"critical": "bad", "warn": "alert"}.get(tight["level"], "text")
        cue = {"critical": "!!", "warn": "!"}.get(tight["level"], "")
        if mode in {"bars", "short_bars"}:
            pieces.append((" " + _used_bar(quota_bar(100 - tight["used"], unicode), unicode), tone))
        pieces.append((" %s%%%s" % (tight["used"], cue), tone))
        if tight["stale"]:
            pieces.append(("~" if terse else " stale " + _age_text(provider), "dim"))
        resets = tight["resets_in"]
        if mode != "minimal" and isinstance(resets, str):
            pieces.append((" " + resets, "dim"))
    if mode in {"bars", "plain"}:
        pieces.append((" %s live" % _seat_count(provider), "dim"))
    elif mode in {"short_bars", "short"}:
        pieces.append((" ·" + _seat_count(provider) if unicode else " x" + _seat_count(provider), "dim"))
    return pieces


def _used_bar(left_bar, unicode):
    """quota_bar fills with what is left; this strip says "used", so the same cells read the other way round."""
    full, empty = ("▮", "▯") if unicode else ("#", "-")
    filled = left_bar.count(full)
    cells = len(left_bar) - (0 if unicode else 2)
    return (full * (cells - filled) + empty * filled) if unicode else "[%s%s]" % (full * (cells - filled), empty * filled)


def _row(pieces, width):
    text = "".join(p for p, unused in pieces)
    if display_width(text) <= width:
        return {"text": text, "spans": pieces}
    from panel_view import clipped
    return {"text": clipped(text, width)}


def strip(report, width, height, unicode=True, now=None):
    """Board rows for the provider strip: [{"text", "spans"?}], one row unless the pane is wide and tall."""
    now = time.time() if now is None else now
    data = snapshot(report, now)["providers"]
    sep = " │ " if unicode else " | "
    dot = " · " if unicode else " / "
    if width >= 80 and height >= 24:
        rows = []
        for key, title, unused_short, unused_letter, hue in PROVIDERS:
            provider = data[key]
            tight = _tightest(provider)
            account = provider["account"]
            name = title + (" (%s)" % account if isinstance(account, str) else "")
            pieces = [((name + " " * 12)[:max(12, len(name) + 1)], hue)]
            if tight is None:
                reasons = provider["windows_unreported"]
                reason = reasons.get("weekly") or next(iter(reasons.values()), "no reading")
                pieces.append(("— " if unicode else "-- ", "dim"))
                pieces.append((reason, "dim"))
                if provider["reading"]["stale"]:
                    pieces.append((dot + ("written %s ago" if reason == STALE_HUD else "stale %s") % _age_text(provider), "dim"))
            else:
                tone = {"critical": "bad", "warn": "alert"}.get(tight["level"], "text")
                cue = {"critical": " !! critical", "warn": " ! past your line"}.get(tight["level"], "")
                pieces.append((_used_bar(quota_bar(100 - tight["used"], unicode), unicode) + " ", tone))
                pieces.append(("%s %s%% used%s" % (tight["name"], tight["used"], cue), tone))
                if tight["stale"]:
                    pieces.append((dot + "stale %s" % _age_text(provider), "dim"))
                if isinstance(tight["resets_in"], str):
                    pieces.append((dot + "resets in " + tight["resets_in"], "dim"))
                others = [w for w in provider["windows"] if w is not tight]
                for other in others:
                    pieces.append((dot + "%s %s%%" % (other["name"], other["used"]), "dim"))
            pieces.append((dot + "%s live" % _seat_count(provider), "dim"))
            rows.append(_row(pieces, width))
        return rows
    variants = [("bars", 100), ("plain", 72), ("short_bars", 56), ("short", 56), ("minimal", 0)]
    for mode, floor in variants:
        if width < floor:
            continue
        glue = dot if mode == "minimal" else sep
        built = []
        for key, unused_title, unused_short, unused_letter, unused_token in PROVIDERS:
            built.append(_segment(data[key], key, mode, unicode, now))
        pieces = []
        for index, part in enumerate(built):
            pieces += ([(glue, "dim")] if index else []) + part
        if display_width("".join(p for p, unused in pieces)) <= width or mode == "minimal":
            return [_row(pieces, width)]
    return []


def _value(item):
    return "unreported (%s)" % item["unreported"] if isinstance(item, dict) and "unreported" in item else str(item)


def text(snap):
    lines = ["Provider usage (read from local files; no provider was queried; stale after %d min)" %
             (snap["fresh_seconds"] // 60)]
    if snap["held"]:
        lines.append("Held frame: readings below were not refreshed")
    for key, unused_title, unused_short, unused_letter, unused_token in PROVIDERS:
        provider = snap["providers"][key]
        lines.append("%s" % provider["name"])
        lines.append("  account: " + _value(provider["account"]))
        for window in provider["windows"]:
            reset = window["resets_in"]
            age = window["age_seconds"]
            lines.append("  %-7s %s%% used%s; resets %s; age %s; source %s%s" % (
                window["name"], window["used"], " [%s]" % window["level"] if window["level"] in {"warn", "critical"} else "",
                "in " + reset if isinstance(reset, str) else _value(reset),
                _remaining(age) if isinstance(age, (int, float)) else _value(age), window["source"],
                "; STALE" if window["stale"] else ""))
        for name, reason in provider["windows_unreported"].items():
            lines.append("  %-7s unreported (%s)" % (name, reason))
        lines.append("  context: " + (_value(provider["context_used"]) if isinstance(provider["context_used"], dict)
                                      else "%s%% used" % provider["context_used"]))
        today, recent = provider["tokens"]["today"], provider["tokens"]["recent_7d"]
        lines.append("  tokens today: " + _value(today))
        lines.append("  tokens 7d: " + (_value(recent) if "unreported" in recent else
                                         "%s calls, in %s, cache read %s, out %s (%s)" % (
                                             recent["calls"], recent["input"], recent["cache_read"], recent["output"],
                                             recent["source"])))
        share = provider["cache_read_share"]
        lines.append("  cache read share: " + (_value(share) if "unreported" in share else
                                               "%s%% of the %s (%s)" % (share["percent"], share["scope"], share["source"])))
        seats = provider["seats"]
        lines.append("  live seats: " + (_value(seats) if "unreported" in seats else
                                         "%d (%d main, %d workers)" % (seats["live"], seats["main"], seats["workers"])))
    return "\n".join(lines)
