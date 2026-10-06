"""Bounded terminal presentation of the existing read-only dashboard contract."""

import unicodedata

from dashboard_projection import clean, count, display_width, fit, listing, mapping, number, wrap_label as wrapped
from panel_routing import policy

PALETTE = {"main": "\033[1;38;5;173m", "worker": "\033[36m", "review": "\033[35m",
           "good": "\033[32m", "alert": "\033[33m", "bad": "\033[31m", "dim": "\033[90m"}
PROVIDER_NAMES = {"codex": "Codex", "claude": "Claude Code"}
TERMINAL_STATES = {"done", "failed", "cancelled", "timed_out", "abandoned"}
TASK_NAMES = {"peer-delegate": "Implement patch", "delegate": "Implement patch",
              "ask": "Investigate / advise", "consult": "Investigate / advise",
              "review": "Review changes", "peer-review": "Review changes"}
MODEL_NAMES = {"gpt-6-sol": "Sol", "gpt-6-luna": "Luna", "gpt-6-astra": "Astra",
               "claude-opus-5-5": "Opus 5.5", "claude-sonnet-5-5": "Sonnet 5.5",
               "claude-fable-5-1": "Fable 5.1"}
PURPOSE_NAMES = {"implement": "Implement patch", "explain": "Explain code",
                 "investigate": "Inspect source", "review": "Review changes", "advise": "Advise decision"}
LIVE_STATES = {"starting", "working", "verifying", "live marker"}
ATTENTION_STATES = {"blocked", "waiting_input", "waiting_approval", "review", "orphaned",
                    "failed", "timed_out", "abandoned"}


def activity(state, frame=None, unicode=True):
    if state in LIVE_STATES:
        return (("⠋⠙⠹⠸" if unicode else "|/-\\")[frame % 4]
                if frame is not None else "●" if unicode else "*")
    return {"done": "✓" if unicode else "+", "failed": "!", "timed_out": "!",
            "blocked": "!", "waiting_input": "?", "waiting_approval": "?",
            "review": "◇" if unicode else "o"}.get(state, "·" if unicode else "-")


def location_label(value):
    if not value:
        return "location unrecorded"
    return value.replace("worktree:oh-my-setting-delegate.", "worktree ").replace("worktree:", "worktree ")


def status_alerts(report):
    alerts = []
    if mapping(report.get("collection")).get("ok") is not True:
        alerts.append("! Collection degraded - some state unknown")
    error = mapping(report.get("room")).get("error")
    if error:
        alerts.append("! Room: " + clean(error, 160))
    return alerts


def clipped(text, width):
    # Preserve layout spaces; clean() deliberately collapses them in data labels.
    text = "".join(" " if ch in "\t\n\r" else "?" if unicodedata.category(ch)[0] == "C"
                   else ch for ch in str(text)[:2000])
    if width >= 3:
        return fit(text, width)
    result = ""
    for char in text:
        if display_width(result + char) > width:
            break
        result += char
    return result


def tone(state):
    if state in {"failed", "timed_out", "abandoned", "orphaned", "unresolved"}:
        return "bad"
    if state in {"waiting_input", "waiting_approval", "blocked"}:
        return "alert"
    if state in {"review", "verifying"}:
        return "review"
    return "dim" if state in TERMINAL_STATES else "worker"


def worker_rows(report):
    """Only explicit panel metadata identifies advisors and main ownership."""
    rows = []
    attempts = mapping(report.get("attempts"))
    active = list(listing(attempts.get("active_recent")))
    seen = {row.get("attempt_id") for row in active}
    active += [row for row in listing(attempts.get("recent"))
               if row.get("state") not in TERMINAL_STATES and row.get("attempt_id") not in seen]
    locations = {mapping(row.get("panel")).get("location") for row in active}
    for row in listing(report.get("delegations")):
        if row.get("location") and row["location"] in locations:
            continue
        state = "live marker" if row.get("live") else "orphaned"
        detail = "role: " + (row.get("role") or "unrecorded")
        model = "model: unrecorded"
        if row.get("requested_model"):
            model = "requested: " + row["requested_model"]
        if row.get("reasoning_effort"):
            model += " | effort: " + row["reasoning_effort"]
        rows.append({"state": state, "provider": row.get("provider") or "unknown", "work": detail,
                     "task": row.get("task_id") or "unrecorded", "model": model,
                     "name": MODEL_NAMES.get(row.get("requested_model"), row.get("requested_model")) or
                             PROVIDER_NAMES.get(row.get("provider"), "Unknown"),
                     "role": "worker", "source": "marker", "parent": None,
                     "location": row.get("location"), "effort": row.get("reasoning_effort"), "access": None})
    # A running council owns its seats: same task and parent, shown as one debate.
    councils = {(row.get("task_id"), row.get("parent_attempt_id")) for row in active
                if row.get("tool") == "panel-council"}
    seats = {}
    for row in active:
        key = (row.get("task_id"), row.get("parent_attempt_id"))
        if key in councils and row.get("tool") != "panel-council":
            seats[key] = seats.get(key, 0) + 1
    for row in active:
        metadata = mapping(row.get("panel"))
        key = (row.get("task_id"), row.get("parent_attempt_id"))
        if row.get("tool") == "panel-council":
            rows.append({"state": row.get("state") or "unknown", "provider": row.get("provider") or "unknown",
                         "work": "Debate: " + (metadata.get("label") or "council"),
                         "task": row.get("task_id") or "unrecorded", "model": "4 seats / 2 families",
                         "name": "%s seat(s) answering" % seats.get(key, 0),
                         "role": "council", "source": "council", "parent": row.get("parent_attempt_id"),
                         "location": None, "effort": None, "access": None})
            continue
        if metadata.get("role") == "main":
            continue
        detail = TASK_NAMES.get(row.get("tool"), row.get("tool") or "Work unrecorded")
        operation = next((item for item in listing(mapping(report.get("operations")).get("recent"))
                          if row.get("attempt_id") and item.get("attempt_id") == row["attempt_id"]), {})
        model = ("served: " + operation["served_model"] if operation.get("served_model") else
                 "selected: " + operation["selected_model"] if operation.get("selected_model") else
                 "selected: " + metadata["model"] if metadata.get("model") else
                 "model: unrecorded")
        model_id = operation.get("served_model") or operation.get("selected_model") or metadata.get("model")
        rows.append({"state": row.get("state") or "unknown", "provider": row.get("provider") or "unknown",
                     "work": "Seat answering" if key in councils else
                             metadata.get("label") or PURPOSE_NAMES.get(metadata.get("purpose"), detail),
                     "task": row.get("task_id") or "unrecorded", "model": model,
                     "name": MODEL_NAMES.get(model_id, model_id) or PROVIDER_NAMES.get(row.get("provider"), "Unknown"),
                     "role": "council" if key in councils else metadata.get("role") or "worker",
                     "source": "attempt",
                     "parent": row.get("parent_attempt_id") if metadata.get("role") else None,
                     "location": metadata.get("location"),
                     "effort": metadata.get("effort"), "access": metadata.get("access"),
                     "owner": metadata.get("owner")})
    rank = {"failed": 0, "orphaned": 0, "blocked": 1, "waiting_approval": 1, "waiting_input": 1}
    return sorted(rows, key=lambda row: (row["source"] != "council", rank.get(row["state"], 2)))


def hierarchy(report, rows, main_attempt=None, include_empty=True):
    """Parent IDs establish ownership; legacy calls retain an unlinked root."""
    attempts = mapping(report.get("attempts"))
    mains = {a.get("attempt_id"): a for a in listing(attempts.get("recent")) +
             listing(attempts.get("active_recent")) if a.get("attempt_id")
             and mapping(a.get("panel")).get("role") == "main"}
    roots = []
    parents = list(dict.fromkeys(r.get("parent") for r in rows))
    if include_empty:
        parents += [ident for ident, a in mains.items() if ident not in parents
                    and a.get("state") and a["state"] not in TERMINAL_STATES]
    for ident in parents:
        children = [r for r in rows if r.get("parent") == ident]
        main = mains.get(ident, {})
        metadata = mapping(main.get("panel"))
        model = metadata.get("model")
        title = ("MAIN / " + (MODEL_NAMES.get(model, model) or
                 PROVIDER_NAMES.get(main.get("provider"), "recorded owner")) if main else
                 "LINKED OWNER / metadata unavailable" if ident else "UNLINKED")
        groups = [(role, [r for r in children if r["role"] == key]) for role, key in
                  (("COUNCIL", "council"), ("ADVISORS", "advisor"), ("REVIEWERS", "reviewer"),
                   ("WORKERS", "worker"))]
        roots.append({"id": ident, "title": title, "label": metadata.get("label"),
                      "state": main.get("state"), "groups": [(g, rs) for g, rs in groups if rs]})
    if main_attempt:
        return [r for r in roots if r["id"] == main_attempt]
    return roots


def model_usage(report):
    """Group the retained call artifacts once; lifecycle totals overlap them."""
    groups, seen = {}, set()
    for row in listing(mapping(report.get("operations")).get("recent")):
        if row.get("kind") not in {"ask", "call", "consult", "review", "delegate"}:
            continue
        ident = row.get("event_id")
        if ident and ident in seen:
            continue
        if ident:
            seen.add(ident)
        ambiguous = row.get("model_attribution") == "ambiguous" or row.get("fallback_used") is True
        attribution = "mixed" if ambiguous else "served" if row.get("served_model") else "selected" if row.get("selected_model") else "unknown"
        model = None if ambiguous else row.get("served_model") or row.get("selected_model")
        key = (row.get("provider"), model, attribution)
        item = groups.setdefault(key, {"name": "Mixed models" if ambiguous else MODEL_NAMES.get(model, model) or
            PROVIDER_NAMES.get(row.get("provider"), "Unknown"), "attribution": attribution,
            "records": 0, "tokens": 0, "token_reports": 0, "cost": 0, "cost_reports": 0})
        item["records"] += 1
        tokens, cost = count(row.get("tokens")), number(row.get("cost_usd"))
        if tokens is not None:
            item["tokens"] += tokens
            item["token_reports"] += 1
        if cost is not None:
            item["cost"] += cost
            item["cost_reports"] += 1
    return list(groups.values())


def usage_labels(report, costs=False):
    def amount(value):
        return (str(value // 1000000) + "." + str(value % 1000000 // 100000) + "m" if value >= 1000000 else
                str(value // 1000) + "." + str(value % 1000 // 100) + "k" if value >= 10000 else str(value))
    labels = []
    for row in model_usage(report):
        tokens = amount(row["tokens"]) if row["token_reports"] else "?"
        if 0 < row["token_reports"] < row["records"]:
            tokens += "+?"
        suffix = "(sel)" if row["attribution"] == "selected" else "(?)" if row["attribution"] == "unknown" else ""
        label = "%s%s %s tok / %sx" % (row["name"], suffix, tokens, row["records"])
        if costs:
            cost = "$%.4g" % row["cost"] if row["cost_reports"] and number(row["cost"]) is not None else "$?"
            if 0 < row["cost_reports"] < row["records"]:
                cost += "+?"
            label += " / " + cost
        labels.append(label)
    return labels


def box_edge(title, width, unicode=True, kind="top"):
    left, right, bar = (("╭", "╮", "─") if kind == "top" else
                        ("├", "┤", "─") if kind == "divider" else ("╰", "╯", "─")) if unicode else ("+", "+", "-")
    label = " " + clipped(title, max(1, width - 6)) + " " if title else ""
    return left + bar + label + bar * max(0, width - display_width(label) - 3) + right


def box_row(text, width, unicode=True):
    edge = "│" if unicode else "|"
    text = clipped(text, width - 4)
    return edge + " " + text + " " * max(0, width - display_width(text) - 3) + edge


def call_groups(groups, width, slots, compact, frame, unicode, limit):
    """Reserve each role's separator before distributing title space."""
    result, shown = [], 0
    content = max(0, slots - len(groups))
    for index, (name, records) in enumerate(groups):
        share = content // (len(groups) - index)
        rows = []
        wide = compact and width >= 80
        minimum = 1 if wide else 2 if compact else 3
        target = min(len(records), share // minimum, limit - shown)
        remaining = share
        for offset, row in enumerate(records[:target]):
            reserve = (target - offset - 1) * minimum
            title_limit = 1 if compact else min(3 if width < 54 else 2, remaining - reserve - 2)
            meta = "[%s] %s%s" % (row["state"], row["name"], " / " + row["access"] if row["access"] else "")
            if wide:
                meta = clipped(meta, 32)
                title_width = width - 8 - display_width(meta)
                text = clipped(row["work"], title_width)
                rows.append(("%s %s%s  %s" % (activity(row["state"], frame, unicode), text,
                    " " * max(0, title_width - display_width(text)), meta), tone(row["state"])))
                remaining -= 1
                shown += 1
                continue
            title = wrapped(row["work"], width - 6, max(1, title_limit))
            rows.extend(("%s %s" % (activity(row["state"], frame, unicode), line) if n == 0 else "  " + line, None)
                        for n, line in enumerate(title))
            rows.append(("  " + meta, tone(row["state"])))
            if not compact:
                detail = ("@ " + location_label(row["location"]) + " / " + (row["effort"] or "auto")
                          if row["location"] else row["model"])
                if width >= 70:
                    detail += " | task: " + row["task"]
                rows.append(("  " + detail, "dim"))
            remaining -= len(title) + (1 if compact else 2)
            shown += 1
        omitted = len(records) - target
        title = "%s (%s)%s" % (name, len(records), " / +%s hidden" % omitted if omitted else "")
        result.append((box_edge(title, width, unicode, "divider"), "worker" if name == "WORKERS" else "review"))
        result.extend((box_row(text, width, unicode), style) for text, style in rows)
        content -= len(rows)
    return result, shown


def render(report, provider, width=100, height=28, color=False, unicode=True,
           previous=None, availability=None, menu=False, managed=False, main_attempt=None, frame=None,
           view="auto", attention_only=False, navigation=None):
    width, height = max(1, min(200, width)), max(1, height)
    if view == "tree" or (view == "auto" and mapping(report.get("room")).get("participants")
                          and width >= 28 and height >= 12):
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation)
    if view == "graph" and mapping(report.get("room")).get("participants") and width >= 76 and height >= 36:
        from room_view import render_graph
        return render_graph(report, width, height, color, unicode, frame, menu, attention_only)
    lines = []

    def add(text="", style=None):
        value = clipped(text, width)
        if color and style:
            value = PALETTE[style] + value + "\033[0m"
        lines.append(value)

    footer = []
    if menu:
        if width >= 54:
            footer = ["MAIN    1 Codex/Sol  2 Claude/Opus  3 Resume  4 Main",
                      "TASK    t Auto task  5 Explain  6 Implement  p CLIs",
                      "JUDGE   a Advisor    7 Review    c Council",
                      "RESULT  r Read  h Chats  f Finalize  n Retry delivery",
                      "VIEW    o Rooms     g Graph     v Density  b Attention  9 Refresh  q Quit/detach"]
        else:
            footer = ["1 Codex/Sol  2 Claude/Opus", "3 Resume  4 Main  t Auto task", "5 Explain  6 Implement",
                      "a Advisor  7 Review  c Council  p CLI peers",
                      "r Results  h Chats  f Finalize  n Retry app",
                      "o Rooms  g Graph  v Density  b Attention", "9 Refresh  q Quit"]
        if managed:
            footer.append("tmux: window picker / next / detach")
    footer.extend(["OMS exits not acceptance; native subagents unobserved"] if width >= 54
                  else ["OMS records only; not acceptance", "Native subagents unobserved"])
    if height < 20:
        footer = (["1 Codex  2 Claude  8 Details  q Quit"] if menu else []) + ["Exits not acceptance"]
    alerts = status_alerts(report)
    footer_space = max(0, height - int(menu) - 1 - len(alerts))
    footer = footer[-footer_space:] if footer_space else []
    budget = max(0, height - len(footer) - int(menu))
    repo = mapping(report.get("repo"))
    add(("✳ " if unicode else "* ") + "OMS " + ("control panel" if menu else "dashboard") +
        " / " + (repo.get("name") or "repository"), "main")
    for alert in alerts:
        add(alert, "bad")
    if height >= 20:
        from panel_metrics import header_rows
        for line in header_rows(report, width):
            add(line, "dim")
    if height >= 34:
        add(repo.get("branch") or "branch unknown", "dim")

    attempts = mapping(report.get("attempts"))
    all_rows = worker_rows(report)
    other = sum(1 for row in all_rows if main_attempt and row["parent"] != main_attempt)
    rows = [row for row in all_rows if not main_attempt or row["parent"] == main_attempt]
    hidden = sum(1 for row in rows if attention_only and row["state"] not in ATTENTION_STATES)
    if attention_only:
        rows = [row for row in rows if row["state"] in ATTENTION_STATES]
    roots = hierarchy(report, rows, main_attempt, include_empty=not attention_only)
    main = next((row for row in listing(attempts.get("active_recent")) + listing(attempts.get("recent"))
                 if main_attempt and row.get("attempt_id") == main_attempt), {})
    metadata = mapping(main.get("panel"))
    if main_attempt and not roots:
        roots = [{"id": main_attempt, "title": "MAIN / " + PROVIDER_NAMES.get(provider, provider),
                  "label": metadata.get("label"), "state": main.get("state"), "groups": []}]

    labels = usage_labels(report, costs=height >= 40)
    usage_title = "USAGE / recent 8 repo records"
    if width >= 80 and height < 30:
        strip = "USAGE recent8 / "
        used = 0
        for index, label in enumerate(labels):
            suffix = " / +%s models" % (len(labels) - index - 1) if index < len(labels) - 1 else ""
            candidate = strip + (" | " if used else "") + label
            if display_width(candidate + suffix) > width:
                break
            strip = candidate
            used += 1
        if used < len(labels):
            strip += " / +%s models" % (len(labels) - used)
        elif not labels:
            strip += "Usage unavailable" if mapping(report.get("operations")).get("available") is False else "unreported; native unobserved"
        add(strip, "dim")
    elif width >= 28 and height >= 24:
        add(box_edge(usage_title, width, unicode), "dim")
        cap = 3 if height >= 30 else 1
        packed, used = [], 0
        for label in labels:
            text = clipped(label, width - 4)
            if packed and display_width(packed[-1] + " | " + text) <= width - 4:
                packed[-1] += " | " + text
            elif len(packed) < cap:
                packed.append(text)
            else:
                break
            used += 1
        if not packed:
            packed = ["Usage unavailable" if mapping(report.get("operations")).get("available") is False
                      else "No reported usage; native unobserved"]
        if used < len(labels):
            # Keep the model count visible rather than clipping it after a subtotal.
            tail = "+%s models / recent 8 records" % (len(labels) - used)
            packed.append(tail)
        for line in packed:
            add(box_row(line, width, unicode))
        add(box_edge("", width, unicode, "bottom"), "dim")
    else:
        add("USAGE: " + (labels[0] if labels else "unreported"), "dim")

    room = mapping(report.get("room"))
    if room and height >= 20:
        add("ROOM / " + (clean(room.get("title") or room.get("id"), 160) or "unavailable"), "main")
        from room_view import nodes
        mains = [member for member in nodes(report) if member.get("role") == "main"]
        if mains:
            names = [MODEL_NAMES.get(member.get("model"), member.get("model")) or member.get("provider", "unknown")
                     for member in mains[:2]]
            add("Joined mains %s / %s%s" % (len(mains), ", ".join(names),
                " +%s" % (len(mains) - 2) if len(mains) > 2 else ""), "dim")
        if view == "graph":
            add("Graph needs 76x36 / showing list", "dim")
        add("MAILBOX / pending %s / consumed %s / answered %s" % (room.get("pending_count", "?"),
            room.get("received_count", "?"), room.get("answered_count", "?")), "dim")
        publications = mapping(room.get("publications"))
        if publications:
            add("APP DELIVERY / " + " | ".join("%s: %s%s" %
                (target, receipt.get("status", "unknown"), "?" if receipt.get("delivery_unknown") else "")
                for target, receipt in list(publications.items())[-3:]), "dim")
    if not main_attempt:
        preset = policy()["main"].get(provider, {})
        if not any(root.get("id") for root in roots):
            add("NEXT MAIN / %s / %s / %s (preset)" % (
                PROVIDER_NAMES.get(provider, provider), MODEL_NAMES.get(preset.get("model"), preset.get("model")) or "native",
                preset.get("effort") or "native settings"), "main")
        if not roots:
            add("Goal: " + (mapping(report.get("goal")).get("text") or "No goal recorded"))

    acceptance = mapping(report.get("acceptance"))
    counts = mapping(acceptance.get("counts"))
    if acceptance.get("available"):
        evidence = "REPO ACCEPT  %s/%s verified" % (counts.get("verified", 0), acceptance.get("total", "?"))
        extras = "  ".join("%s=%s" % (key, counts[key]) for key in ("failed", "missing", "stale") if counts.get(key))
        if extras:
            evidence += "  " + extras
        evidence_tone = "bad" if counts.get("failed") else "good" if acceptance.get("complete") else "alert"
    else:
        evidence, evidence_tone = "REPO ACCEPT  evidence unavailable", "alert"
    status_lines = [(evidence, evidence_tone)]
    items = sorted(listing(mapping(report.get("attention")).get("items")), key=lambda row: row.get("priority") or "P9")
    if items and height >= (30 if menu else 24):
        status_lines.append(("Repo: " + (items[0].get("summary") or "Attention required"), "alert"))
    if previous is not None and height >= 24:
        status_lines.append(("Last: " + previous, "dim"))
    if availability is not None and width >= 54 and height >= 34:
        status_lines.append(("CLI  " + "  ".join("%s: %s" % (name, "installed" if ready else "missing")
                                              for name, ready in availability.items()), "dim"))
    available = max(0, budget - len(lines) - len(status_lines) - 2)
    minimum = sum(2 + len(root["groups"]) + (3 if main_attempt else int(bool(root.get("label")))) for root in roots)
    compact = view == "compact" or (view == "auto" and minimum + 3 * len(rows) > available)
    add("OMS activity / " + ("attention" if attention_only else "this main" if main_attempt else "repository") +
        " / " + ("compact" if compact else "detail"), "alert" if attention_only else "dim")
    space = max(0, budget - len(lines) - len(status_lines) - 1)
    roots.sort(key=lambda r: (r["id"] is None, r["id"] or ""))
    plans = []
    for root in roots:
        prelude = []
        title = root["title"] + (" / " + root["state"] if root["state"] else " / ownership unrecorded")
        if main_attempt:
            title = "MAIN / " + PROVIDER_NAMES.get(main.get("provider") or provider, provider)
            prelude += [(line, None) for line in wrapped("Goal: " + (metadata.get("label") or "Native task not recorded"),
                                                       width - 4, 1 if compact else 2)]
            prelude.append(("%s %s / %s / %s" % (activity(main.get("state"), frame, unicode),
                MODEL_NAMES.get(metadata.get("model"), metadata.get("model")) or "model unrecorded",
                metadata.get("effort") or "effort unrecorded", main.get("state") or "unobserved"), "dim"))
        elif root.get("label") and not compact:
            prelude = [(line, None) for line in wrapped(root["label"], width - 4, 2)]
        if not root["groups"]:
            prelude.append(("No active calls need attention" if attention_only else "No active child calls", "dim"))
        records = [row for _, rs in root["groups"] for row in rs]
        per_call = 1 if compact and width >= 80 else 2 if compact else 3
        minimum = 2 + len(prelude) + len(root["groups"])
        desired = minimum + sum((per_call + (0 if compact else len(wrapped(row["work"], width - 6,
                        3 if width < 54 else 2)) - 1)) for row in records[:8])
        plans.append({"title": title, "prelude": prelude, "minimum": minimum, "desired": desired,
                      "records": records, "slots": 1})
    remaining = max(0, space - len(plans))
    priority = sorted(range(len(plans)), key=lambda i: (not any(r["state"] in ATTENTION_STATES
                      for r in plans[i]["records"]), i))
    for index in priority:
        plan = plans[index]
        need = plan["desired"] - 1
        if width >= 28 and remaining >= plan["minimum"] - 1:
            extra = min(remaining, need)
            plan["slots"] += extra
            remaining -= extra
    shown = 0
    for root_index, root in enumerate(roots):
        if space <= 0:
            break
        plan = plans[root_index]
        title, prelude = plan["title"], plan["prelude"]
        allocated = min(space, plan["slots"])
        if width < 28 or allocated < plan["minimum"]:
            warning = next((r for r in plan["records"] if r["state"] in ATTENTION_STATES), None)
            leading = warning or next(iter(plan["records"]), None)
            summary = title + (" / " + leading["work"] if leading else "")
            summary += " / " + ", ".join("%s %s" % (g, len(rs)) for g, rs in root["groups"])
            add(summary, "alert" if warning else "main" if root["id"] else "dim")
            space -= 1
            continue
        add(box_edge(title, width, unicode), "main" if root["id"] else "dim")
        for text, style in prelude:
            add(box_row(text, width, unicode), style)
        content, count_shown = call_groups(root["groups"], width, allocated - len(prelude) - 2,
                                          compact, frame, unicode, 8 - shown)
        for text, style in content:
            add(text, style)
        add(box_edge("", width, unicode, "bottom"), "main" if root["id"] else "dim")
        space -= len(prelude) + len(content) + 2
        shown += count_shown
    if not roots and space > 0:
        add("No active calls need attention" if attention_only and attempts.get("available") else
            "No active records" if attempts.get("available") else "Worker state unavailable", "dim")
    if len(rows) > shown or other or hidden:
        parts = ["+%s more" % (len(rows) - shown)] if len(rows) > shown else []
        parts += ["%s filtered" % hidden] if hidden else []
        parts += ["%s outside" % other] if other else []
        overflow = " / ".join(parts) + " / 8 Details"
        if display_width(overflow) > width:
            parts = ["+%s" % (len(rows) - shown)] if len(rows) > shown else []
            parts += ["f%s" % hidden] if hidden else []
            parts += ["out%s" % other] if other else []
            overflow = " / ".join(parts) + " / 8 Details"
        add(overflow, "dim")

    recent = [row for row in listing(attempts.get("recent")) if row.get("state") in TERMINAL_STATES
              and (not attention_only or row.get("state") in ATTENTION_STATES)
              and (not main_attempt or row.get("parent_attempt_id") == main_attempt or row.get("attempt_id") == main_attempt)]
    remaining = budget - len(lines) - len(status_lines)
    if remaining >= 2 and recent:
        add("RECENT / not acceptance", "dim")
        for row in recent[:min(2, remaining - 1)]:
            operation = next((item for item in listing(mapping(report.get("operations")).get("recent"))
                              if row.get("attempt_id") and item.get("attempt_id") == row["attempt_id"]), {})
            data = mapping(row.get("panel"))
            model = operation.get("served_model") or operation.get("selected_model") or data.get("model")
            source = "served" if operation.get("served_model") else "selected"
            work = (data["role"] + ": " + PURPOSE_NAMES.get(data.get("purpose"), row.get("tool") or "work")
                    if data else TASK_NAMES.get(row.get("tool"), row.get("tool") or "work"))
            add("[%s] %s / %s%s" % (row.get("state"), row.get("provider") or "unknown", work,
                  " / %s: %s" % (source, model) if model else ""), tone(row.get("state")))
    if len(lines) > max(0, budget - len(status_lines)):
        # Tiny panes prioritize summaries; no bordered card is emitted there.
        lines = lines[:max(0, budget - len(status_lines))]
    for text, style in status_lines[:budget]:
        add(text, style)
    lines = lines[:budget]
    for line in footer:
        add(line, "dim" if line.startswith(("OMS", "Native", "tmux", "Exits")) else "main")
    return "\n".join(lines[:height - int(menu)])

def render_results(report, width=100, compact=False):
    """Scroll ordinary text: summaries first, then retained answers and evidence."""
    width = max(8, min(200, width))
    lines = [clipped("OMS / Results", width)]
    def add(text, indent="", limit=12):
        for line in wrapped(str(text), width - len(indent), limit):
            lines.append(indent + line)
    rows = listing(report.get("rows"))
    if not rows:
        add("No retained results. Finalize a task to record its owner summary.")
    for number, row in enumerate(rows, 1):
        lines.append("")
        if compact:
            add("%s. %s [%s]" % (number, row.get("title") or row.get("task_id") or "Task",
                                row.get("outcome") or "owner decision pending"), limit=3)
            add("Verify: %s / App: %s" % (mapping(row.get("verification")).get("status") or "not run",
                                        row.get("delivery") or "not_requested"), "   ", 2)
            continue
        add(row.get("title") or row.get("task_id") or "Task", limit=3)
        add("Task: " + (row.get("task_id") or "unknown"), limit=3)
        add("Outcome: %s | process: %s | verify: %s" % (
            row.get("outcome") or "owner decision pending", row.get("process_state") or "unrecorded",
            mapping(row.get("verification")).get("status") or "not run"))
        add("App: %s%s" % (row.get("delivery") or "not_requested",
                           " / delivery uncertain; inspect before retry" if row.get("delivery_unknown") else ""))
        if row.get("issue"):
            add(row["issue"])
        if row.get("summary"):
            for paragraph in row["summary"].splitlines()[:24]:
                add(paragraph, "  ", 6)
        else:
            add("Owner summary pending. Model exit does not establish acceptance.", "  ")
        for call in listing(row.get("calls")):
            model = call.get("served_model") or call.get("selected_model") or call.get("requested_model")
            add("%s / %s / exit %s" % (MODEL_NAMES.get(model, model) or "unrecorded model",
                call.get("kind") or "call", call.get("exit", "unknown")), "  ", 3)
            if call.get("answer"):
                for paragraph in call["answer"].splitlines()[:12]:
                    add(paragraph, "    ", 6)
        if row.get("artifact"):
            add("Result: " + row["artifact"], limit=4)
        for ref in listing(row.get("evidence")):
            add("Evidence: " + str(ref.get("path", "unavailable")), limit=4)
    lines.append("")
    add(report.get("coverage") or "Retained OMS evidence")
    if report.get("truncated"):
        add("Recent tasks only; select a task ID for its retained answers.")
    return "\n".join(lines)
