"""Connected room tree and a frame-local map of navigation targets."""

from dashboard_projection import clean, display_width, listing, mapping
from panel_metrics import header_rows
from panel_view import (ATTENTION_STATES, MODEL_NAMES, PALETTE, activity, box_edge, box_row, clipped,
                        menu_rows, status_alerts, tone, usage_labels, worker_rows, wrapped)
from room_view import nodes, readable, said, window_order


def render_tree(report, width, height, color=False, unicode=True, frame=None, menu=False,
                attention_only=False, main_attempt=None, navigation=None, summary=False, managed=False):
    navigation = navigation if navigation is not None else {}
    sep = " · " if unicode else " / "
    selected = navigation.get("selected")
    collapsed = navigation.get("collapsed", set())
    room = mapping(report.get("room"))
    if height <= int(menu):
        navigation.update(hits=[], items=[], offset=0, viewport=0, positions={}, room_id=room.get("id"))
        return ""
    members = nodes(report)
    current = next((m["participant"] for m in members if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)), None)
    # This window's main leads so its team stays on screen; the rest keep the F6/F7 window order.
    mains = sorted(window_order(report, [m for m in members if m.get("role") == "main"]),
                   key=lambda m: m["participant"] != current)
    calls = sorted([m for m in members if m.get("role") != "main"],
                   key=lambda m: (m.get("seq", 0), m["participant"]))
    owners = {ident: m for m in mains for ident in (m["participant"], m.get("attempt")) if ident}
    for row in worker_rows(report):
        if row["source"] == "council" and row["parent"] in owners:
            owner = owners[row["parent"]]
            calls.append({"role": "council", "parent": row["parent"], "state": row["state"],
                          "title": row["work"], "model": row["name"],
                          "_action": ("debate", (row["task"], owner["participant"]))})
    if attention_only:
        calls = [m for m in calls if m["state"] in ATTENTION_STATES]
    prefix, end, stem = ("├─ ", "└─ ", "│  ") if unicode else ("|- ", "`- ", "|  ")
    rows, items = [], []
    boxed = width >= 28 and height >= 16
    content_width = width - 4 if boxed else width

    def add(text, style=None, action=None, fold=False, divider=False):
        rows.append({"text": clipped(text, content_width), "style": style, "action": action,
                     "fold": fold, "divider": divider})
        if action:
            items.append(action)

    roots = set(owners)
    for n, main in enumerate(mains):
        last_main = n == len(mains) - 1
        root_stem = "  " + ("   " if last_main else stem)
        attached = [m for m in calls if m.get("parent") and
                    m["parent"] in {main["participant"], main.get("attempt")}]
        action = ("chat", main["participant"])
        folded = main["participant"] in collapsed
        toggle = ("▸" if folded else "▾") if unicode else (">" if folded else "v")
        toggle = toggle if attached else " "
        model = MODEL_NAMES.get(main.get("model"), main.get("model")) or main.get("provider", "unknown")
        state = "exited" if main["state"] == "done" else main["state"]
        said_state = state if state == "exited" else said(state)
        label = "%s %s%s %s%s" % (toggle, "" if summary or width < 60 else end if last_main else prefix,
            activity(state, frame, unicode), "this window" + sep if main["participant"] == current else "", model,
        )
        status = said_state + (sep + "%s calls" % len(attached) if folded else "")
        add(label if width < 60 else label + sep + status, "codex" if main.get("provider") == "codex" else "main",
            action, bool(attached))
        if width < 60:
            add("  " + status, tone(state), action)
        title = main.get("title")
        if title and title != "Native task not recorded":
            for line in wrapped(title, max(1, content_width - 7), 2):
                add(root_stem + "  " + line, None, action)
        if not folded:
            groups = [(role, [m for m in attached if m.get("role") == role])
                      for role in ("council", "advisor", "reviewer", "worker")]
            groups = [(role, children) for role, children in groups if children]
            for group_index, (role, children) in enumerate(groups):
                last_group = group_index == len(groups) - 1
                heading = "COUNCIL" if role == "council" else role.upper() + "S"
                add(root_stem + (end if last_group else prefix) + heading + " (%s)" % len(children),
                    "worker" if role == "worker" else "review")
                indent = root_stem + ("   " if last_group else stem)
                for child_index, child in enumerate(children):
                    branch = end if child_index == len(children) - 1 else prefix
                    model = MODEL_NAMES.get(child.get("model"), child.get("model")) or child.get("provider", "unknown")
                    label = child.get("title") or child["participant"]
                    child_action = child.get("_action") or ("result", child["participant"])
                    add(indent + branch + "%s %s%s%s" % (
                        activity(child["state"], frame, unicode), model, sep, said(child["state"])),
                        tone(child["state"]), child_action)
                    for line in wrapped(label, max(1, content_width - display_width(indent) - 3), 2):
                        add(indent + "   " + line, None, child_action)
        if n < len(mains) - 1:
            add("  │" if unicode else "  |", "dim")
    unlinked = [m for m in calls if m.get("parent") not in roots]
    if unlinked:
        add("Calls with no known main", "alert")
        for n, child in enumerate(unlinked):
            add((end if n == len(unlinked) - 1 else prefix) + "%s %s%s%s" % (
                activity(child["state"], frame, unicode), child.get("title") or child["participant"], sep,
                said(child["state"])),
                tone(child["state"]), ("result", child["participant"]))
    if not mains and not calls:
        add("No main connected" if room.get("id") else "Select a work room", "dim")
        if menu:
            add("Start with [1] Codex or [2] Claude", "main")
    tasks = listing(room.get("repo_tasks"))
    if tasks:
        add("Repository tasks", "worker", divider=True)
        for task in tasks:
            action = ("task", task.get("id"))
            add("%s%s%s" % (said(task.get("state")), sep, task.get("title") or task.get("id")),
                tone(task.get("state")), action)
    acceptance = mapping(report.get("acceptance"))
    if acceptance.get("available"):
        add("Plan: %s of %s tasks verified" % (mapping(acceptance.get("counts")).get("verified", 0),
            acceptance.get("total", "?")), "good" if acceptance.get("complete") else "alert")
    for message in listing(room.get("messages"))[-2:]:
        add("Room log" + sep + (readable(message.get("text"), " ") or ""), "dim")
    if any(main.get("owns") for main in mains):
        add("Scopes" + sep + "declared, not locks", "dim", divider=True)
    for main in mains:
        if main.get("owns"):
            add("  " + (main.get("provider") or "main") + ": " + ", ".join(main["owns"]), "dim")
    footer = menu_rows(width, height, unicode, navigation.get("menu_help", False), managed) if menu else []
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    if not menu:
        footer.append(("[v] Expand  [q] " + ("Chat" if managed else "Quit")) if summary else
                      "[v] Collapse  Esc Back / arrows / Enter  [q] " + ("Chat" if managed else "Quit"))
    if len(footer) > height - int(menu) - 4:
        footer = [clipped("1 Codex  2 Claude  ? More  q Quit" if menu else "v Expand  q Quit", width)]
    budget = max(0, height - int(menu) - len(footer))
    heading = "OMS / " + (clean(room.get("title") or room.get("id")) or "Work room")
    head = [{"text": clipped(heading, width), "style": "main"}]
    head += [{"text": clipped(alert, width), "style": "bad"} for alert in status_alerts(report)]
    if boxed and height >= 20:
        usage = usage_labels(report)
        labels = usage[:2] if summary else usage
        if summary and len(usage) > 2:
            labels += ["+%s models / expand for more" % (len(usage) - 2)]
        labels = labels or ["No reported calls"]
        maximum = max(2, budget - len(head) - 9)
        if len(labels) > maximum:
            labels = labels[:maximum - 1] + ["+%s models / expand for more" % (len(labels) - maximum + 1)]
        wide = width >= 84 and not summary
        left = (content_width + 8) // 2
        metric_lines = header_rows(report, left if wide else content_width)
        content = []
        if wide:
            content.append(("PROVIDER LIMITS".ljust(left) + "  MODEL CALLS / recent 8", "dim"))
            for index in range(max(len(metric_lines), len(labels))):
                metric = metric_lines[index] if index < len(metric_lines) else ""
                label = labels[index] if index < len(labels) else ""
                metric = clipped(metric, left)
                content.append((metric + " " * (left - display_width(metric)) + "  " + label, None))
        else:
            content = [(line, None) for line in metric_lines]
            content += [("MODEL CALLS / recent 8", "dim")] + [(line, None) for line in labels]
        head.append({"text": box_edge("USAGE / W used, C left" if content_width < 40 else "USAGE", width, unicode), "style": "dim"})
        head += [{"text": box_row(line, width, unicode), "style": style} for line, style in content]
        head.append({"text": box_edge("", width, unicode, "bottom"), "style": "dim"})
    elif budget - len(head) >= 5:
        head += [{"text": clipped(line, width), "style": "dim"} for line in header_rows(report, width)]
    if budget - len(head) >= 5:
        mailbox = "Messages: %s unread" % room.get("pending_count", 0)
        if not summary:
            mailbox += " · %s read · %s answered" % (room.get("received_count", 0), room.get("answered_count", 0))
        head.append({"text": clipped(mailbox, width),
            "style": "alert" if room.get("pending_count") else "dim"})
    # The viewport owns its borders, so scrolling never leaves an open card.
    if boxed and budget - len(head) < 3:
        head = head[:1 + len(status_alerts(report))]
    head = head[:budget]
    room_budget = max(0, budget - len(head) - (2 if boxed else 0))
    body = rows
    positions = {}
    for index, row in enumerate(body):
        if row.get("action"):
            positions.setdefault(row["action"], index)
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - room_budget))
    if len(body) > room_budget and room_budget:
        room_budget -= 1
        offset = min(offset, max(0, len(body) - room_budget))
    geometry = (width, height, menu, summary)
    if room_budget and selected in positions and (navigation.get("geometry") != geometry or
                                                 navigation.get("viewport") != room_budget):
        position = positions[selected]
        if position < offset:
            offset = position
        elif position >= offset + room_budget:
            offset = position - room_budget + 1
    shown = list(head)
    if boxed:
        shown.append({"text": box_edge("Activity%s%s mains%s%s calls%s" % (
            sep, len(mains), sep, len(calls), sep + "attention" if attention_only else ""), width, unicode), "style": "main"})
    shown += body[offset:offset + room_budget]
    if len(body) > room_budget and budget > len(head):
        shown.append({"text": clipped("%s-%s / %s rows / %s" % (offset + 1, min(len(body), offset + room_budget), len(body),
                      "j/k to scroll" if menu else "scroll for more"), content_width),
                      "style": "dim", "action": None})
    if boxed:
        shown.append({"text": box_edge("", width, unicode, "bottom"), "style": "main"})
    output, hits = [], []
    unique = list(dict.fromkeys(items))
    for y, row in enumerate(shown):
        value = row["text"]
        style = row.get("style")
        inner = boxed and len(head) < y < len(shown) - 1
        if not color and row.get("action") and row["action"] == selected:
            value = clipped(value, max(0, content_width - 2)) + " <" if content_width >= 2 else "<"
        if inner:
            value = box_edge(value, width, unicode, "divider") if row.get("divider") else box_row(value, width, unicode)
        if color and row.get("action") and row["action"] == selected:
            value = PALETTE[style or "head"] + "\033[7m" + value + "\033[0m"
        elif color and style:
            value = PALETTE[style] + value + "\033[0m"
        output.append(value)
        if row.get("action"):
            hits.append({"y": y + 1, "x1": 1, "x2": width, "action": row["action"], "fold": row.get("fold", False)})
    output += [PALETTE["dim"] + clipped(line, width) + "\033[0m" if color and
               line.startswith(("╭", "╰", "+", "OMS records")) else clipped(line, width)
               for line in footer]
    navigation.update(hits=hits, items=unique, offset=offset, body_rows=len(body), viewport=room_budget,
                      room_id=room.get("id"), positions=positions, geometry=geometry)
    return "\n".join(output)


def render_detail(report, detail, width, height, navigation):
    heading = ["OMS / " + detail["title"]] + header_rows(report, width)
    if "report" in detail:
        from panel_view import render_results
        text = render_results(detail["report"], width)
    else:
        text = detail["text"]
    body = [part for line in text.splitlines()
            for part in wrapped(line, width, max(1, len(line)), max_chars=max(400, len(line)))]
    budget = max(0, height - len(heading) - 2)
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - budget))
    navigation.update(hits=[], items=[], offset=offset, viewport=budget, bands=[])
    lines = heading[:max(0, height - 2)] + body[offset:offset + budget]
    lines += ["%s-%s / %s rows" % (offset + 1, min(len(body), offset + budget), len(body)),
              "Esc: tree  wheel / PgUp / PgDn: scroll  q: quit watcher"]
    return "\n".join(clipped(line, width) for line in lines[-height:])
