"""Bounded terminal presentation of the existing read-only dashboard contract."""

import os
import re
import unicodedata

from dashboard_projection import (clean, count, display_width, fit, listing, mapping, number, use_unicode,
                                  wrap_label as wrapped)
from panel_routing import policy



def light_theme():
    """OMS_PANEL_THEME=light|dark wins; else a light COLORFGBG background (7 or 15) means light."""
    chosen = os.environ.get("OMS_PANEL_THEME", "").lower()
    if chosen in ("light", "dark"):
        return chosen == "light"
    return os.environ.get("COLORFGBG", "").rpartition(";")[2] in ("7", "15")


# 256-colour hues: Claude warm, Codex blue, workers teal, advisors lavender; darker shades on light themes.
PALETTE = {name: "\033[%s38;5;%sm" % ("1;" if name in ("main", "codex", "head") else "", shade[light_theme()])
           for name, shade in {"main": (209, 166), "codex": (75, 25), "worker": (80, 30), "review": (141, 97),
                               "head": (117, 24), "good": (114, 28), "alert": (221, 130), "bad": (203, 160),
                               "dim": (245, 242)}.items()}
PROVIDER_NAMES = {"codex": "Codex", "claude": "Claude Code", "pi": "Pi"}
TERMINAL_STATES = {"done", "failed", "cancelled", "timed_out", "abandoned"}
TASK_NAMES = {"peer-delegate": "Implement patch", "delegate": "Implement patch",
              "ask": "Investigate / advise", "consult": "Investigate / advise",
              "review": "Review changes", "peer-review": "Review changes"}
MODEL_NAMES = {"gpt-6.1-sol": "Sol 6.1", "gpt-6-sol": "Sol 6", "gpt-6-luna": "Luna 6", "gpt-6-astra": "Astra 6",
               "claude-opus-5-5": "Opus 5.5", "claude-sonnet-5-5": "Sonnet 5.5",
               "claude-fable-5-1": "Fable 5.1", "claude-haiku-5-5": "Haiku 5.5"}
PURPOSE_NAMES = {"implement": "Implement patch", "explain": "Explain code",
                 "investigate": "Inspect source", "review": "Review changes", "advise": "Advise decision",
                 "research": "Look up sources",
                 # Role strategies record their role as the purpose (routing keeps the four board kinds).
                 "builder": "Build", "refactorer": "Refactor", "documenter": "Document", "integrator": "Integrate",
                 "performance-engineer": "Measure performance", "release-engineer": "Prepare release",
                 "architect": "Structure design", "critic": "Find failure cases", "verifier": "Verify against request",
                 "security-auditor": "Security audit", "reviewer": "Review changes", "advisor": "Advise decision",
                 "researcher": "Look up sources", "worker": "Work"}
LIVE_STATES = {"starting", "working", "verifying", "live marker"}
ATTENTION_STATES = {"blocked", "waiting_input", "waiting_approval", "review", "orphaned",
                    "failed", "timed_out", "abandoned"}


def activity(state, frame=None, unicode=True):
    if state in LIVE_STATES:
        return (("⠋⠙⠹⠸" if unicode else "|/-\\")[frame % 4]
                if frame is not None else "●" if unicode else "*")
    return {"idle": "○" if unicode else "o", "done": "✓" if unicode else "+", "failed": "!", "timed_out": "!",
            "blocked": "!", "waiting_input": "?", "waiting_approval": "⚑" if unicode else "A",
            "review": "◇" if unicode else "o"}.get(state, "·" if unicode else "-")


def location_label(value):
    if not value:
        return "location unrecorded"
    return value.replace("worktree:oh-my-setting-delegate.", "worktree ").replace("worktree:", "worktree ")


def main_occupied(report, members=None):
    """Return attempt-shaped evidence; joined mains need confirmed termination before replacement."""
    attempt = next((row for row in listing(mapping(report.get("attempts")).get("active_recent"))
                    if mapping(row.get("panel")).get("role") == "main"
                    and row.get("state") not in TERMINAL_STATES), None)
    if attempt is not None:
        return attempt
    if members is None:
        from room_view import nodes
        members = nodes(report)
    member = next((row for row in members if row.get("role") == "main"
                   and row.get("state") not in TERMINAL_STATES), None)
    if member is not None:
        return {"attempt_id": member.get("attempt"), "state": member.get("state"),
                "panel": {"role": "main", "room_id": mapping(report.get("room")).get("id"),
                          "room_participant": member.get("participant")}}
    return None


def status_alerts(report):
    alerts = []
    if mapping(report.get("collection")).get("ok") is not True:
        alerts.append("! Collection degraded - some state unknown")
    error = mapping(report.get("room")).get("error")
    if error:
        alerts.append("! Room: " + clean(error, 160) + " - o selects a room, 9 retries")
    if report.get("capacity_alerts"):
        alerts.extend(report["capacity_alerts"])
    return alerts


def clipped(text, width):
    # Preserve layout spaces; clean() deliberately collapses them in data labels.
    text = "".join(" " if ch in "\t\n\r" else "?" if unicodedata.category(ch)[0] == "C"
                   else ch for ch in str(text)[:2000] if ch not in "\ufe0e\ufe0f")
    if width >= 3:
        return fit(text, width)
    result = ""
    for char in text:
        if display_width(result + char) > width:
            break
        result += char
    return result


def message_rows(text, width):
    """Preserve message line breaks and wrap by cells without dropping spaces or adding ellipses."""
    rows = []
    for line in str(text).splitlines():
        line = clipped(line, display_width(line))
        value, cells = "", 0
        for char in line:
            step = display_width(char)
            if value and cells + step > width:
                rows.append(value)
                value, cells = "", 0
            value += char
            cells += step
        rows.append(value)
    return rows


def tone(state):
    if state in {"failed", "timed_out", "abandoned", "orphaned", "unresolved"}:
        return "bad"
    if state == "cancelled":
        return "dim"
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
    from room_view import nodes
    projected = {m["attempt"]: m for m in nodes(report)
                 if m.get("role") == "main" and m.get("attempt")}
    roots = []
    parents = list(dict.fromkeys(r.get("parent") for r in rows))
    if include_empty:
        parents += [ident for ident, a in mains.items() if ident not in parents
                    and a.get("state") and a["state"] not in TERMINAL_STATES]
    for ident in parents:
        children = [r for r in rows if r.get("parent") == ident]
        main = mains.get(ident, {})
        metadata = mapping(main.get("panel"))
        model = projected[ident].get("model") if ident in projected else metadata.get("model")
        title = ("MAIN / " + (MODEL_NAMES.get(model, model) or
                 PROVIDER_NAMES.get(main.get("provider"), "recorded owner")) if main else
                 "LINKED OWNER / metadata unavailable" if ident else "No known main")
        groups = [(role, [r for r in children if r["role"] == key]) for role, key in
                  (("COUNCIL", "council"), ("ADVISORS", "advisor"), ("REVIEWERS", "reviewer"),
                   ("RESEARCHERS", "researcher"), ("WORKERS", "worker"))]
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


def usage_words(report, unicode=True, costs=False):
    """usage_labels() in plain words for display; the labels themselves stay as other callers read them."""
    mark = "×" if unicode else "x"
    return [re.sub(r" (\S+) tok / (\d+)x", lambda m: " %s tokens %s%s" % (m.group(1), mark, m.group(2)),
                   label.replace("(sel)", "").replace("(?)", " (model unconfirmed)"))
            for label in usage_labels(report, costs)]


def box_edge(title, width, unicode=True, kind="top"):
    left, right, bar = (("╭", "╮", "─") if kind == "top" else
                        ("├", "┤", "─") if kind == "divider" else ("╰", "╯", "─")) if unicode else ("+", "+", "-")
    label = " " + clipped(title, max(1, width - 6)) + " " if title else ""
    return left + bar + label + bar * max(0, width - display_width(label) - 3) + right


def box_row(text, width, unicode=True):
    edge = "│" if unicode else "|"
    text = clipped(text, width - 4)
    return edge + " " + text + " " * max(0, width - display_width(text) - 3) + edge


def menu_rows(width, height, unicode=True, expanded=False, managed=False, keys=True, occupied=False, installed=None):
    """Starting a main, rooms, refresh and quit; the board's own keys are not repeated."""
    quit_label = "Detach" if managed else "Quit"
    other = "[o] Rooms  [k] Capacity  [9] Refresh  [?] More  [q] " + quit_label
    harnesses = [("claude", "[1] Claude"), ("pi", "[2] Pi"), ("codex", "[0] Codex")]
    start_keys = [label for name, label in harnesses if installed is None or name in installed]
    starts = "  ".join(start_keys + (["[3] Resume", "[t] Task"] if start_keys else []))

    def wrap_groups(text, row_width, limit):
        groups = [group.strip() for group in re.findall(r"\[[^]]+\](?: [^[]+)?", text)]
        if not groups:
            return wrapped(text, row_width, limit)
        rows, current = [], ""
        for group in groups:
            candidate = group if not current else current + " " + group
            if current and display_width(candidate) > row_width:
                rows.append(current)
                current = group
            else:
                current = candidate
        if current:
            rows.append(current)
        return [clipped(line, row_width) for line in rows[:limit]]
    if height < 20 or width < 28:
        options = (["[o] Rooms [k] Capacity [9] Refresh [?] [q] " + quit_label,
                    "[o] [k] [9] [?] [q]"] if occupied else
                   [" ".join(start_keys + ["[?]", "[q] " + quit_label]),
                    " ".join(re.findall(r"\[[^]]+\]", " ".join(start_keys)) + ["[?]", "[q]"])])
        compact = next((line for line in options if display_width(line) <= width), "[?] [q]")
        return [clipped(compact, width)]
    groups = ([] if occupied else [("START", starts)] if start_keys else []) + [("OTHER", other)]
    if expanded:
        groups += [("OPEN", "[h] Chats  [r] Results  [g] Graph  [z] Panel"),
                   ("TERMINAL", "[s] Terminals and jobs"),
                   ("WORK", "[5] Explain  [6] Implement  [e] Researcher  [a] Advisor  [7] Review  [c] Council"),
                   ("SETUP", "[4] Main provider  [p] CLIs  [8] Dashboard"),
                   ("RESULT", "[f] Finalize  [n] Retry delivery")]
    mode = "all shortcuts" if expanded else "press a key" if keys else "key + Enter"
    lines = [box_edge("Actions" + (" · " if unicode else " / ") + mode, width, unicode)]
    if width < 60 and not expanded:
        compact = ([] if occupied else ["  ".join(start_keys), "[3] Resume  [t] Task"] if start_keys else []) + [other]
        for line in compact:
            lines += [box_row(part, width, unicode) for part in wrap_groups(line, width - 4, 3)]
        return lines + [box_edge("", width, unicode, "bottom")]
    indent = 9 if expanded else 7
    for label, keys_text in groups:
        for index, line in enumerate(wrap_groups(keys_text, max(1, width - indent - 5), 5 if not expanded else len(keys_text))):
            lines.append(box_row((label.ljust(indent) if index == 0 else " " * indent) + line, width, unicode))
    return lines + [box_edge("", width, unicode, "bottom")]


INBOX_LIMIT = 12
FAILED_WORDS = {"failed": "failed", "timed_out": "timed out", "abandoned": "abandoned", "orphaned": "lost its process"}
WAITING_WORDS = {"waiting_approval": "waiting for approval", "waiting_input": "waiting for input"}
ONBOARDING = "Press {starts} to start the sole main{codex}"


def age_label(stamp, now=None):
    import calendar
    import time
    try:
        then = calendar.timegm(time.strptime(str(stamp)[:19], "%Y-%m-%dT%H:%M:%S"))
    except ValueError:
        return ""
    seconds = max(0, int((time.time() if now is None else now) - then))
    return ("<1m" if seconds < 60 else "%sm" % (seconds // 60) if seconds < 3600
            else "%sh" % (seconds // 3600) if seconds < 86400 else "%sd" % (seconds // 86400))


def inbox_items(report, unicode=True):
    """What the person must act on, newest first: failures, waits, broadcasts from mains, patches to admit."""
    from room_view import (closed_tasks, failure_handled, handled_results, main_names, main_state, message_address,
                           nodes, readable)
    members = nodes(report)
    titles = main_names(members)
    name = lambda m: MODEL_NAMES.get(m.get("model"), m.get("model")) or m.get("provider") or "unknown"
    mains = {ident: m for m in members if m.get("role") == "main"
             for ident in (m["participant"], m.get("attempt")) if ident}
    attempts = mapping(report.get("attempts"))
    stamps = {row.get("attempt_id"): row.get("updated_at") for key in ("recent", "active_recent")
              for row in listing(attempts.get(key))}
    finalized, awaiting = mapping(report.get("finalized")), set(listing(report.get("awaiting_admission")))
    read_results, closed = handled_results(mapping(report.get("room"))), closed_tasks(report)
    successors = mapping(report.get("message_successors"))
    byid = {m["participant"]: m for m in members}
    label = lambda m: clean(titles.get(m["participant"]) or name(m))
    address = lambda ident: message_address(ident, members, successors,
                                           lambda target: label(byid[target]) if target in byid else clean(target))

    def who(member):
        if member.get("role") == "main":
            return titles.get(member["participant"], name(member)) + " main"
        parent = mains.get(member.get("parent"))
        return "%s %s" % (name(member), member.get("role")) + (
            " for " + titles.get(parent["participant"], name(parent)) if parent else "")
    items = []
    for member in members:
        state = main_state(member) if member.get("role") == "main" else member["state"]
        if state in FAILED_WORDS and not failure_handled(report, member, read_results, closed):
            what, shown, style = FAILED_WORDS[state], state, "bad"
        elif state in WAITING_WORDS:
            what, shown, style = WAITING_WORDS[state], state, "alert"
        elif state == "done" and member["participant"] in awaiting:
            what, shown, style = "patch awaits admission", "review", "review"
        elif state == "review" and finalized.get(member.get("task_id")) not in {"completed", "accepted"}:
            what, shown, style = "awaiting decision", "review", "review"
        else:
            continue
        title = clean(member.get("title"), 80)
        items.append({"stamp": stamps.get(member.get("attempt")) or "", "glyph": activity(shown, None, unicode),
                      "who": who(member), "what": what + (": " + title if title and title != "Native task not recorded" else ""),
                      "style": style, "action": ("chat" if member.get("role") == "main" else "result", member["participant"]),
                      "row_id": ("call", member["participant"])})
    from panel_messages import exited_mail
    open_ids = {mapping(q).get("id") for q in listing(mapping(report.get("room")).get("open_questions"))}
    for index, message in enumerate(listing(mapping(report.get("room")).get("messages"))):
        sender = mains.get(message.get("sender"))
        if (sender and message.get("recipient") == "all" and message.get("message_kind") != "answer"
                and not message.get("answered") and message.get("pending_for")):
            text = readable(message.get("text"), " ")
            items.append({"stamp": message.get("ts") or "", "glyph": ">" if not unicode else "»",
                          "who": who(sender), "what": "to all: " + clean(text, 80),
                          "style": "alert", "action": ("chat", sender["participant"]),
                          "row_id": ("message", str(message.get("id")) if message.get("id") else ("position", index))})
    room = mapping(report.get("room"))
    pending = room.get("pending_messages") if "pending_messages" in room else room.get("messages")
    for index, message in enumerate(listing(pending)):
        if exited_mail(message, byid) and message.get("id") not in open_ids:
            target = successors.get(message["recipient"], message["recipient"])
            asker = byid.get(message.get("sender"), {})
            items.append({"stamp": message.get("ts") or "", "glyph": "?" if message.get("message_kind") == "question" else ">",
                          "who": ("Question" if message.get("message_kind") == "question" else "Message") +
                                 " from " + (label(asker) if asker else clean(message.get("sender"))),
                          "what": "to " + address(message["recipient"]) + ": " + clean(readable(message.get("text"), " "), 80),
                          "style": "alert", "action": ("chat", target),
                          "row_id": ("message", str(message.get("id")) if message.get("id") else ("position", index))})
    import room as room_module
    for index, question in enumerate(listing(mapping(report.get("room")).get("open_questions"))):
        question, minutes = mapping(question), room_module.age(mapping(question).get("ts")) // 60
        asker, target = byid.get(question.get("sender")), byid.get(question.get("recipient"))
        if (asker and target and target.get("role") == "main"
                and (minutes * 60 >= room_module.PERSON_AFTER or target.get("state") in TERMINAL_STATES)):
            recipient = successors.get(target["participant"], target["participant"])
            items.append({"stamp": question.get("ts") or "", "glyph": "?", "who": "Question from " + label(asker),
                          "what": "to " + address(target["participant"]), "style": "alert",
                          "action": ("chat", recipient),
                          "row_id": ("question", str(question.get("id")) if question.get("id") else ("position", index))})
    failure_statuses = {"failed", "timeout", "timed_out", "error", "rejected", "unavailable", "failed_command",
                        "rpc_error", "persistence_failure", "absent", "archived", "busy", "invalid_timeout", "uncertain",
                        "unsupported_platform", "unbound", "ambiguous", "unsafe_endpoint", "empty_message", "oversized"}
    failed = [mapping(receipt) for receipt in mapping(mapping(report.get("room")).get("publications")).values()
              if mapping(receipt).get("status") in failure_statuses]
    if failed:
        items.append({"stamp": max(receipt.get("ts") or "" for receipt in failed), "glyph": "!",
                      "who": "Main wake-up delivery failed",
                      "what": "(unconfirmed)" if any(receipt.get("delivery_unknown") for receipt in failed) else "",
                      "style": "bad", "action": None, "row_id": ("delivery", "wake-up")})
    return sorted(items, key=lambda item: item["stamp"], reverse=True)


def render_inbox(report, width, height, color, unicode, managed, previous, navigation):
    """The control window's menu pane: only what needs the person, then the few actions that start work."""
    navigation = navigation if navigation is not None else {}
    sep = " · " if unicode else " / "
    room = mapping(report.get("room"))
    items = inbox_items(report, unicode)
    occupied = bool(main_occupied(report) or room.get("error") or mapping(report.get("collection")).get("ok") is not True
                    or report.get("capacity_stale") or navigation.get("capacity_stale") or navigation.get("room_poll_unknown"))
    navigation["main_occupied"] = occupied
    footer = menu_rows(width, height, unicode, navigation.get("menu_help", False), managed,
                       keys=not navigation.get("line_input"), occupied=occupied, installed=navigation.get("installed"))
    if managed and height >= 20:
        footer.append("F5 Back / F12 keys / Ctrl-b d detach")
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    if len(footer) > height - 1 - 4:
        footer = menu_rows(width, 1, unicode, managed=managed, occupied=occupied, installed=navigation.get("installed"))
    budget = max(0, height - 1 - len(footer))
    head = [("✳ " if unicode else "* ") + "OMS control panel / " +
            (clean(room.get("title") or room.get("id")) or mapping(report.get("repo")).get("name") or "repository")]
    styles = ["head"]
    from panel_metrics import usage_line
    from room_view import goal_banner
    head.append(usage_line(report, width, unicode))
    styles.append("dim")
    full = goal_banner(report, width, unicode)
    # Header, usage and goal box are pinned; the goal box shrinks to one row before any of them is dropped.
    for text, style in full if height >= 20 and budget - 2 - len(full) >= 6 else goal_banner(report, width, unicode, True):
        head.append(text)
        styles.append(style)
    pinned = len(head)
    for alert in status_alerts(report):
        head.append(alert)
        styles.append("bad")
    if previous is not None and height >= 12:
        last = message_rows("Last: " + previous, width) if "\n" in previous or "oms panel --release-main " in previous else ["Last: " + previous]
        if len(last) > 1:
            keep = max(pinned, budget - 3 - len(last))
            head, styles = head[:keep], styles[:keep]
            # Recovery commands must survive the optional-header trimming below.
            pinned = len(head) + len(last)
        head.extend(last)
        styles.extend(["dim"] * len(last))
    if not occupied:
        installed = navigation.get("installed")
        names = [(name, label) for name, label in (("claude", "1 (Claude)"), ("pi", "2 (Pi)"))
                 if installed is None or name in installed]
        starts = " or ".join(label for _, label in names)
        codex = "; 0 opens Codex" if installed is None or "codex" in installed else ""
        onboarding = ONBOARDING.format(starts=starts, codex=codex) if starts else ""
        protected = onboarding.replace("1 (Claude)", "1\u00a0(Claude)").replace("2 (Pi)", "2\u00a0(Pi)")
        tokens = re.findall(r"1\u00a0\(Claude\)|2\u00a0\(Pi\)|\S+", protected)
        onboarding_rows, current = [], ""
        for token in tokens:
            candidate = token if not current else current + " " + token
            if current and display_width(candidate) > width:
                onboarding_rows.append(current)
                current = token
            else:
                current = candidate
        if current:
            onboarding_rows.append(current)
        for line in onboarding_rows[:3] if onboarding else []:
            line = clipped(line.replace("\u00a0", " "), width)
            head.append(line)
            styles.append("main")
    keep = max(pinned, budget - 3)
    head, styles = head[:keep], styles[:keep]
    boxed = width >= 28 and budget - len(head) >= 4
    available = max(0, budget - len(head) - (2 if boxed else 0))
    shown, hidden = items[:INBOX_LIMIT], max(0, len(items) - INBOX_LIMIT)
    # Rows can share a destination. Keep the selection on the row while actions still name the destination.
    seen = {}
    row_ids = []
    for item in shown:
        key = item["row_id"]
        occurrence = seen.get(key, 0)
        seen[key] = occurrence + 1
        row_ids.append((key, occurrence))
    selected = navigation.get("selected")
    selected_row = navigation.get("selected_row")
    if selected_row not in row_ids or selected != shown[row_ids.index(selected_row)]["action"]:
        selected_row = next((row_ids[n] for n, item in enumerate(shown) if item["action"] == selected),
                            row_ids[0] if shown else None)
    selected = navigation["selected"] = shown[row_ids.index(selected_row)]["action"] if selected_row else None
    navigation["selected_row"] = selected_row
    capacity = available - (1 if len(shown) + bool(hidden) > available else 0) if shown else 1
    capacity = max(0, capacity)
    index = row_ids.index(selected_row) if selected_row else 0
    start = min(max(0, index - capacity + 1), max(0, len(shown) - capacity))
    window = shown[start:start + capacity]
    content_width = width - 4 if boxed else width
    body = []
    if not shown:
        body.append(("Nothing needs you right now", "dim", None))
    for row_id, item in zip(row_ids[start:start + capacity], window):
        tail = item["tail"] if "tail" in item else age_label(item["stamp"])
        tail = sep + tail if tail else ""
        text = clipped("%s %s%s" % (item["glyph"], item["who"], sep + item["what"] if item["what"] else ""),
                       max(1, content_width - display_width(tail) - 2)) + tail
        body.append((text, item["style"], item["action"], row_id))
    more = len(shown) - len(window) + hidden
    if shown and more and available > len(window):
        body.append(("+%s more%s" % (more, sep + "arrows, wheel or page keys" if len(shown) > len(window) else ""), "dim", None))
    lines, hits = [], []
    for text, style in zip(head, styles):
        lines.append(clipped(text, width))
        lines[-1] = (PALETTE[style] + lines[-1] + "\033[0m") if color else lines[-1]
    if boxed:
        title = "Needs you" + (sep + "%s" % len(items) if items else "") + (sep + "Up/Down, Enter opens" if items and not navigation.get("line_input") else "")
        lines.append((PALETTE["alert" if items else "dim"] if color else "") + box_edge(title, width, unicode) + ("\033[0m" if color else ""))
    for entry in body:
        text, style, action = entry[:3]
        row_id = entry[3] if len(entry) > 3 else None
        chosen = row_id is not None and row_id == selected_row
        value = clipped(text, content_width)
        if not color and chosen:
            value = clipped(value, max(0, content_width - 2)) + " <"
        if boxed:
            value = box_row(value, width, unicode)
        if color and chosen:
            value = PALETTE[style] + "\033[7m" + value + "\033[0m"
        elif color:
            value = PALETTE[style] + value + "\033[0m"
        lines.append(value)
        if action is not None:
            hits.append({"y": len(lines), "x1": 1, "x2": width, "action": action, "row_id": row_id})
    if boxed:
        lines.append(box_edge("", width, unicode, "bottom"))
    lines += [clipped(line, width) for line in footer]
    navigation.update(hits=hits, items=[item["action"] for item in shown], row_ids=row_ids,
                      offset=0, viewport=capacity,
                      surface="inbox",
                      room_id=room.get("id"), positions={}, geometry=(width, height, True, False))
    return "\n".join(lines[:height - 1])


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


def room_unreadable(report):
    """Match only the no-room fallback used when collection fails."""
    return mapping(report.get("collection")) == {"ok": False} and not report.get("room")


def render_unavailable(report, width, height, color, unicode, previous, menu, managed, navigation):
    """Bounded status view for a room whose first read has failed."""
    # Nothing from the prior room remains selectable or clickable.
    for key in ("selected", "primary", "preview", "surface", "bands", "band_offsets", "box",
                "full_result", "graph_selected", "main_attempt", "body_rows", "preview_signature", "geometry",
                "selected_row", "row_ids", "detail"):
        navigation.pop(key, None)
    navigation.update(hits=[], items=[], positions={}, offset=0, viewport=0)
    budget = height - int(menu)
    if budget <= 0:
        return ""
    navigation["main_occupied"] = True
    footer = menu_rows(width, height, unicode, navigation.get("menu_help", False), managed,
                       keys=not navigation.get("line_input"), occupied=True) if menu else []
    if menu and managed and height >= 20:
        footer.append("F5 Back / F12 keys / Ctrl-b d detach")
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    if len(footer) > budget - 1:
        footer = menu_rows(width, 1, unicode, managed=managed, occupied=True) if menu else []
    alert = "! Room state unavailable" if width >= 24 else "! Room state unknown" if width >= 20 else "! Unknown" if width >= 9 else "?"
    head = [(("✳ " if unicode else "* ") + "OMS " + ("control panel" if menu else "dashboard") + " / " +
             (clean(mapping(report.get("repo")).get("name")) or "repository"), "head"),
            ("! " + clean(mapping(report.get("room")).get("error"), 160) if mapping(report.get("room")).get("error") else alert, "bad"),
            ("This room could not be read; its work and messages are unknown, not empty.", "dim"),
            (("o selects a room, 9 retries" if menu else "A later refresh may recover it."), "dim")]
    body = max(1, budget - len(footer))
    if previous is not None and height >= 12:
        last = message_rows("Last: " + previous, width) if "\n" in previous or "oms panel --release-main " in previous else ["Last: " + previous]
        head = head[:max(2, body - len(last))] + [(line, "dim") for line in last]
    if body == 1:
        head = [head[1] if width >= 20 else ("?", "bad")]
    lines = []
    for text, style in head[:body] + [(line, "dim" if line.startswith("F5") else "main") for line in footer]:
        text = clipped(text, width)
        lines.append(PALETTE[style] + text + "\033[0m" if color else text)
    return "\n".join(lines[:budget])


def needs_strip(report, width, unicode, limit=3):
    """Pinned attention derived from the same inbox as the control window."""
    items, seen = [], set()
    for item in inbox_items(report, unicode):
        if item["row_id"] not in seen:
            items.append(item)
            seen.add(item["row_id"])
    if not items:
        return []
    sep = " · " if unicode else " / "
    rows = []
    for index, item in enumerate(items[:max(0, limit)]):
        prefix = "Needs you" + sep if index == 0 else "  "
        text = prefix + item["glyph"] + " " + item["who"] + sep + item["what"]
        if display_width(text) > width:
            state = item["what"].split(":", 1)[0].replace("waiting for approval", "needs approval").replace(
                "waiting for input", "needs input")
            who = item["who"].split(" for ", 1)[0]
            if item["row_id"][0] == "call":
                who = who.rsplit(" ", 1)[0]
            text = prefix + who + sep + state
        rows.append({"text": clipped(text, width),
                     "style": item["style"], "action": item["action"]})
    if len(items) > max(0, limit):
        rows.append({"text": clipped("+%s more" % (len(items) - max(0, limit)), width),
                     "style": "alert", "action": ("tab", "needs")})
    return rows


def render(report, provider, width=100, height=28, color=False, unicode=True,
           previous=None, availability=None, menu=False, managed=False, main_attempt=None, frame=None,
           view="auto", attention_only=False, navigation=None):
    width, height = max(1, min(200, width)), max(1, height)
    use_unicode(unicode)
    if navigation is not None:
        navigation.update(tab_strip_visible=False, tab_body_visible=False, help_rows=max(4, height // 3))
    if room_unreadable(report):
        return render_unavailable(report, width, height, color, unicode, previous, menu, managed,
                                  navigation if navigation is not None else {})
    if navigation is not None:
        # Graph-only targets must not leak into a tree drawn from the same navigation.
        for key in ("surface", "bands", "primary"):
            navigation.pop(key, None)
    if menu and view != "graph":
        return render_inbox(report, width, height, color, unicode, managed, previous, navigation)
    if view in ("tree", "summary") or (view == "auto" and mapping(report.get("room")).get("participants")
                          and width >= 20 and height >= 12):
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation, summary=view == "summary", managed=managed,
                           previous=previous)
    if view == "graph" and width >= 76 and height >= 16:
        from room_view import render_graph
        return render_graph(report, width, height, color, unicode, frame, menu, attention_only,
                            main_attempt, navigation, managed, previous=previous)
    if (view == "graph" and navigation is not None and mapping(report.get("room")).get("participants")
            and width >= 20 and height >= 8):
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation, managed=managed, previous=previous)
    lines = []

    def add(text="", style=None):
        value = clipped(text, width)
        if color and style:
            value = PALETTE[style] + value + "\033[0m"
        lines.append(value)

    footer = []
    if menu:
        menu_navigation = navigation if navigation is not None else {}
        occupied = bool(
            menu_navigation.get("room_poll_unknown") or menu_navigation.get("capacity_stale") or report.get("capacity_stale")
            or mapping(report.get("room")).get("error") or mapping(report.get("collection")).get("ok") is not True
            or main_occupied(report))
        menu_navigation["main_occupied"] = occupied
        footer = menu_rows(width, height, unicode, menu_navigation.get("menu_help", False), managed,
                           keys=not menu_navigation.get("line_input"), occupied=occupied, installed=menu_navigation.get("installed"))
        if managed:
            footer.append("F5 control / F12 keys / Ctrl-b d detach")
    sep = " · " if unicode else " / "
    wide = "A finished call is not accepted until its main records it" + sep + "native subagents other than the built-in advisor are not shown"
    if width >= 28:
        footer.extend([wide if display_width(wide) <= width else "Finished is not accepted" + sep + "some subagents not shown"]
                      if width >= 54 else ["Finished is not accepted", "Some native subagents not shown"])
    if height < 20 and width >= 28:
        footer = (footer[:1] if menu else []) + ["Finished is not accepted"]
    alerts = status_alerts(report)
    footer_space = max(0, height - int(menu) - 1 - len(alerts))
    footer = footer[-footer_space:] if footer_space else []
    budget = max(0, height - len(footer) - int(menu))
    repo = mapping(report.get("repo"))
    add("OMS" + sep + (os.path.basename(clean(repo.get("name")) or str(repo.get("path") or "").rstrip("/\\")) or "repository"), "head")
    from panel_metrics import usage_line
    from room_view import goal_banner
    add(usage_line(report, width, unicode), "dim")
    for text, style in goal_banner(report, width, unicode, compact=height < 20):
        add(text, style)
    for alert in alerts:
        add(alert, "bad")
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
    from room_view import main_state, main_words, nodes
    shown_mains = {m.get("attempt"): m for m in nodes(report) if m.get("role") == "main"}
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
        narrow = width < 28
        add(("Room: " if narrow else "ROOM / ") + (clean(room.get("title") or room.get("id"), 160) or "unavailable"), "main")
        from room_view import main_names, nodes
        members = nodes(report)
        mains = [member for member in members if member.get("role") == "main"]
        if mains:
            live_mains = [member for member in mains if member.get("state") not in TERMINAL_STATES]
            if live_mains:
                labels = main_names(members)
                names = [labels[member["participant"]] for member in live_mains[:2]]
                add(("Mains " if narrow else "Joined mains ") + "%s / %s%s" % (len(live_mains), ", ".join(names),
                    " +%s" % (len(live_mains) - 2) if len(live_mains) > 2 else ""), "dim")
            else:
                add(("Mains " if narrow else "Mains: none live (") + ("0 live / %s exited" % len(mains) if narrow else "%s exited)" % len(mains)), "dim")
        if view == "graph" and not narrow:
            add("Graph needs 76x16 / showing list", "dim")
        add(("Unread %s" % room.get("pending_count", "?") if narrow else
             sep.join(["Messages: %s unread" % room.get("pending_count", "?"), "%s read" % room.get("received_count", "?"),
                       "%s answered" % room.get("answered_count", "?")])), "dim")
    if not main_attempt:
        preset = policy()["main"].get(provider, {})
        if not any(root.get("id") for root in roots):
            add(("Next: " + PROVIDER_NAMES.get(provider, provider) if width < 28 else "NEXT MAIN / %s / %s / %s (preset)" % (
                PROVIDER_NAMES.get(provider, provider), MODEL_NAMES.get(preset.get("model"), preset.get("model")) or "native",
                preset.get("effort") or "native settings")), "main")
        if not roots:
            add("Goal: " + (mapping(report.get("goal")).get("text") or "No goal recorded"))

    acceptance = mapping(report.get("acceptance"))
    counts = mapping(acceptance.get("counts"))
    if acceptance.get("available"):
        evidence = ("Accept " if width < 28 else "REPO ACCEPT  ") + "%s/%s%s" % (counts.get("verified", 0), acceptance.get("total", "?"), " ok" if width < 28 else " verified")
        extras = "  ".join("%s=%s" % (key, counts[key]) for key in ("failed", "missing", "stale") if counts.get(key))
        if extras:
            evidence += "  " + extras
        evidence_tone = "bad" if counts.get("failed") else "good" if acceptance.get("complete") else "alert"
    else:
        evidence, evidence_tone = ("Accept unknown" if width < 28 else "REPO ACCEPT  evidence unavailable"), "alert"
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
        shown_main = shown_mains.get(root["id"])
        state_word = main_words(shown_main, unicode) if shown_main else root["state"]
        title = root["title"] + (" / " + state_word if state_word else " / ownership unrecorded")
        if main_attempt:
            title = "MAIN / " + PROVIDER_NAMES.get(metadata.get("harness") or main.get("provider") or provider, provider)
            prelude += [(line, None) for line in wrapped("Goal: " + (metadata.get("label") or "Native task not recorded"),
                                                       width - 4, 1 if compact else 2)]
            prelude.append(("%s %s / %s / %s" % (activity(main_state(shown_main) if shown_main else main.get("state"), frame, unicode),
                MODEL_NAMES.get(metadata.get("model"), metadata.get("model")) or "model unrecorded",
                metadata.get("effort") or "effort unrecorded", state_word or "unobserved"), "dim"))
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
        overflow = " / ".join(parts) + " / v Expand"
        if display_width(overflow) > width:
            parts = ["+%s" % (len(rows) - shown)] if len(rows) > shown else []
            parts += ["f%s" % hidden] if hidden else []
            parts += ["out%s" % other] if other else []
            overflow = " / ".join(parts) + " / v Expand"
        add(overflow, "dim")

    recent = [row for row in listing(attempts.get("recent")) if row.get("state") in TERMINAL_STATES
              and (not attention_only or row.get("state") in ATTENTION_STATES)
              and (not main_attempt or row.get("parent_attempt_id") == main_attempt or row.get("attempt_id") == main_attempt)]
    remaining = budget - len(lines) - len(status_lines)
    if remaining >= 2 and recent:
        add("Recent" if width < 28 else "RECENT / not acceptance", "dim")
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
        add(line, "dim" if line.startswith(("OMS", "Native", "tmux", "A finished", "Finished", "Some")) else "main")
    return "\n".join(lines[:height - int(menu)])

def _result_blocks(text):
    """Classify the reader's list/code/pipe-table subset; retain original lines."""
    source = str(text).split("\n")
    containers, leaf, fence = [], "prose", ""
    paragraph = False
    delimiter_row = -1
    next_nonblank = 0

    def opener(value):
        match = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", value)
        if match and not (match[1][0] == "`" and "`" in match[2]):
            return match[1]
        return ""

    def item(value):
        match = re.match(r"^ {0,3}([-+*]|[0-9]{1,9}[.)])(?= |$)", value)
        if not match:
            return None
        rest = value[match.end():]
        padding = len(rest) - len(rest.lstrip(" "))
        # Five or more spaces start code after one marker-padding space.
        padding = padding if 1 <= padding <= 4 and rest.strip() else 1
        return match, match.end() + padding

    def cells(value, single=False):
        # Escaped pipes belong to their cell, including in inline code spans.
        parts, start, escaped = [], 0, False
        for index, char in enumerate(value):
            if char == "|" and not escaped:
                parts.append(value[start:index])
                start = index + 1
            escaped = char == "\\" and not escaped
        if not parts:
            return [value] if single else []
        parts.append(value[start:])
        if not parts[0].strip():
            parts.pop(0)
        if parts and not parts[-1].strip():
            parts.pop()
        return parts

    def table_start(index, value):
        header = cells(value)
        if not header or index + 1 == len(source):
            return False
        following = source[index + 1].expandtabs(4)
        matched, offset = container_content(following)
        following = following[offset:]
        if matched != len(containers) or len(following) - len(following.lstrip(" ")) > 3:
            return False
        delimiter = cells(following, single=True)
        return len(header) == len(delimiter) and all(
            re.fullmatch(r" *:?-+:? *", cell) for cell in delimiter)

    def quote(value):
        return re.match(r"^ {0,3}> ?", value)

    def block_start(value, table=False):
        return (re.match(r"^ {0,3}#{1,6}(?: |$)", value) or
                re.fullmatch(r" {0,3}(?:\*(?: *\*){2,}|-(?: *-){2,}|_(?: *_){2,}) *", value) or
                re.match(r"^ {0,3}(?:<!--|<\?|<![A-Z]|<!\[CDATA\[|(?i:"
                         r"<(?:script|pre|style|textarea)(?= |>|$)|</?(?:"
                         r"address|article|aside|base|basefont|blockquote|body|caption|center|col|"
                         r"colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|"
                         r"form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|"
                         r"menu|menuitem|nav|noframes|ol|optgroup|option|p|param|search|"
                         r"section|source|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul"
                         r")(?= |/?>|$)))", value) or
                (table and re.fullmatch(
                    r" {0,3}(?:</[A-Za-z][A-Za-z0-9-]* *>|"
                    r"<(?!(?i:script|style|pre|textarea)(?=[ />]))[A-Za-z][A-Za-z0-9-]*"
                    r"(?: +[A-Za-z_:][A-Za-z0-9_.:-]*(?: *= *(?:[^\x00\x09-\x0d \"'=<>`]+|"
                    r"'[^'\x00]*'|\"[^\"\x00]*\"))?)* */?>) *", value)))

    def container_content(line):
        offset = 0
        for matched, entry in enumerate(containers):
            value = line[offset:]
            if entry[0] == "quote":
                mark = quote(value)
                if not mark:
                    return matched, offset
                offset += mark.end()
            elif not value.strip(" "):
                if entry[2]:
                    return matched, offset
            elif len(value) - len(value.lstrip(" ")) >= entry[1]:
                offset += entry[1]
            else:
                return matched, offset
        return len(containers), offset

    for index, original in enumerate(source):
        line = original.expandtabs(4)
        matched, container = container_content(line)
        value = line[container:]
        blank = not value.strip(" ")
        leading = len(value) - len(value.lstrip(" "))
        if leaf == "fence" and matched == len(containers):
            closing = re.fullmatch(r" {0,3}" + re.escape(fence[0]) +
                                   "{%d,} *" % len(fence), value)
            if closing:
                leaf, fence = "prose", ""
            yield original, "code"
            continue
        if leaf == "code" and matched == len(containers):
            if blank and next_nonblank <= index:
                next_nonblank = len(source)
                for following_index in range(index + 1, len(source)):
                    following = source[following_index].expandtabs(4)
                    count, offset = container_content(following)
                    if count != len(containers) or following[offset:].strip(" "):
                        next_nonblank = following_index
                        break
            following = line
            if blank:
                following = source[next_nonblank].expandtabs(4) if next_nonblank < len(source) else ""
            count, offset = container_content(following)
            if count == len(containers) and following[offset:].startswith("    "):
                yield original, "code"
                continue
        if blank:
            del containers[matched:]
            paragraph, leaf = False, "prose"
            yield original, "prose"
            continue

        marker = item(value)
        # Only paragraph text may lazily omit enclosing list indentation.
        # A sibling item ends that paragraph regardless of its start number or content.
        interrupts = opener(value) or marker or quote(value) or block_start(value)
        if matched < len(containers) and paragraph and not interrupts:
            yield original, "prose"
            continue
        if matched < len(containers):
            del containers[matched:]
            paragraph, leaf = False, "prose"
        elif leaf in ("fence", "code"):
            paragraph, leaf = False, "prose"

        if leaf == "table" and (index == delimiter_row or
                (leading <= 3 and not opener(value) and not marker and not quote(value) and
                 not block_start(value, table=True))):
            yield original, "table"
            continue
        if leaf == "table":
            leaf = "prose"

        # Setext underlines close a paragraph before list-marker recognition.
        if paragraph and re.fullmatch(r" {0,3}(?:=+|-+) *", value):
            paragraph = False
            yield original, "prose"
            continue
        while marker or quote(value):
            quoted = quote(value)
            if quoted:
                containers.append(["quote", 0, False])
                container += quoted.end()
                value = line[container:]
                paragraph = False
                marker = item(value)
                continue
            match, content = marker
            if block_start(value):
                break
            if paragraph and (not value[content:].strip() or
                              (match[1][0].isdigit() and int(match[1][:-1]) != 1)):
                break
            container += content
            containers.append(["list", content, not value[content:].strip()])
            value = line[container:]
            paragraph = False
            marker = item(value)
        for entry in containers[:-1] if not value.strip() else containers:
            entry[2] = False
        mark = opener(value)
        if mark:
            leaf, fence, paragraph = "fence", mark, False
            kind = "code"
        elif not paragraph and value.startswith("    "):
            leaf, paragraph = "code", False
            kind = "code"
        elif leading <= 3 and table_start(index, value):
            leaf, paragraph, delimiter_row = "table", False, index + 1
            kind = "table"
        else:
            paragraph = bool(value.strip()) and not block_start(value)
            kind = "prose"
        yield original, kind


def _result_structured(line, width, indent=""):
    from dashboard_projection import ELLIPSIS
    # Keep terminal controls inert without clipping retained printable content.
    line = "".join("?" if ch != "\t" and unicodedata.category(ch)[0] == "C" else ch for ch in line)
    prefix, piece, used = indent, [], 0
    for char in line:
        cells = 8 - (display_width(prefix) + used) % 8 if char == "\t" else display_width(char)
        if used + cells > width - display_width(prefix):
            yield prefix + "".join(piece)
            prefix, piece, used = indent + ("↪ " if ELLIPSIS[0] == "…" else "+ "), [], 0
            cells = 8 - display_width(prefix) % 8 if char == "\t" else cells
        piece.append(char)
        used += cells
    yield prefix + "".join(piece)


def render_results(report, width=100, compact=False, heading=None):
    """Scroll ordinary text, preserving the layout of retained code and tables."""
    from room_view import MACHINE, changed_lines, readable, said
    width = max(8, min(200, width))
    lines = [clipped(heading or "OMS / Results", width)]

    def add(text, indent="", limit=None):
        text = str(text)
        for line in wrapped(text, width - len(indent), limit or max(1, len(text)),
                            max_chars=max(400, len(text))):
            lines.append(indent + line)

    def prose(text, indent=""):
        # readable() strips markdown and leading spaces, so structured pieces bypass it.
        ordinary = []

        def flush():
            for line in ordinary:
                text = readable(line, paragraphs=True)
                if text:
                    spaces = line[:len(line) - len(line.lstrip(" \t"))]
                    safe = clipped(spaces + text, max(2000, display_width(spaces + text)))
                    if display_width(indent + safe) <= width:
                        lines.append(indent + safe)
                    else:
                        add(text, indent + clipped(spaces, max(0, width - len(indent) - 1)))
                elif not line.strip():
                    lines.append("")
            ordinary[:] = []

        for line, kind in _result_blocks(text):
            if kind != "prose":
                flush()
                lines.extend(_result_structured(line, width, indent))
            elif not MACHINE.match(line.strip()):
                ordinary.append(line)
        flush()

    rows = listing(report.get("rows"))
    if not rows:
        add("No result in the retained evidence window." if report.get("truncated") else
            "No retained results. Finalize a task to record its owner summary.")
    for number, row in enumerate(rows, 1):
        lines.append("")
        check = mapping(row.get("verification")).get("status")
        outcome = {"accepted": "Accepted by its main", "completed": "Completed; the main recorded a summary",
                   "failed": "Marked failed by its main"}.get(row.get("outcome"), "Not reviewed by its main yet")
        checks = "checks " + str(check).replace("_", " ") if check and check != "not_run" else "no checks run"
        delivery = row.get("delivery")
        if compact:
            add("%s. %s [%s]" % (number, row.get("title") or row.get("task_id") or "Task", outcome), limit=3)
            add("Verify: " + checks, "   ", 2)
            if delivery and delivery != "not_requested":
                add("App: " + delivery, "   ", 2)
            continue
        calls = listing(row.get("calls"))
        latest = mapping(calls[-1]) if calls else {}
        model = row.get("model") or latest.get("served_model") or latest.get("selected_model") or latest.get("requested_model")
        purpose = PURPOSE_NAMES.get(row.get("purpose")) or TASK_NAMES.get(latest.get("kind")) or PURPOSE_NAMES.get(row.get("role")) or "Work"
        add(" · ".join((MODEL_NAMES.get(model, model) or "unrecorded model", purpose,
                        said(row.get("process_state")), row.get("title") or row.get("task_id") or "Task")))
        add("Task: " + (row.get("task_id") or "unknown"), limit=3)
        add("Outcome: " + outcome + " · " + checks)
        for line in changed_lines(row):
            add(line)
        if delivery and delivery != "not_requested":
            add("App: %s%s" % (delivery, " / delivery uncertain; inspect before retry" if row.get("delivery_unknown") else ""))
        if row.get("issue"):
            add(row["issue"])
        if row.get("summary"):
            prose(row["summary"], "  ")
        else:
            add("Owner summary pending. Model exit does not establish acceptance.", "  ")
        for call in calls:
            model = call.get("served_model") or call.get("selected_model") or call.get("requested_model")
            add("%s / %s / exit %s" % (MODEL_NAMES.get(model, model) or "unrecorded model",
                call.get("kind") or "call", call.get("exit", "unknown")), "  ", 3)
            if call.get("answer"):
                prose(call["answer"], "    ")
                if call.get("answer_truncated"):
                    add("Answer exceeds the retained preview limit; full source: " +
                        str(call.get("artifact") or "unavailable"), "    ")
            if call.get("artifact"):
                add("Answer artifact: " + call["artifact"], "    ")
        if row.get("artifact"):
            add("Result: " + row["artifact"], limit=4)
        for ref in listing(row.get("evidence")):
            add("Evidence: " + str(ref.get("path", "unavailable")), limit=4)
    lines.append("")
    add(report.get("coverage") or "Retained OMS evidence")
    if report.get("truncated"):
        add("Recent tasks only; select a task ID for its retained answers.")
    return "\n".join(lines)
