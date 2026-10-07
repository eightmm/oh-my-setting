"""Connected room tree and a frame-local map of navigation targets."""

from dashboard_projection import clean, display_width, listing, mapping
from panel_metrics import header_rows
from panel_view import (ATTENTION_STATES, PALETTE, activity, box_edge, box_row, clipped,
                        menu_rows, status_alerts, tone, usage_words, worker_rows, wrapped)
from room_view import (action as member_action, call_order, call_span, footer_hints, live_unread,
                       goal_banner, main_names, model_name, native_advisors, nodes, plan_idle, readable, said,
                       settler, spawn_bar, window_order, call_classifier, CALL_GROUPS, declared_status)


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
    names = main_names(report, members)
    settled, _ = settler(report)
    classify = call_classifier(report)
    expanded = navigation.get("expanded_groups", set())
    current = next((m["participant"] for m in members if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)), None)
    # This window's main leads so its team stays on screen; the rest keep the F6/F7 window order.
    mains = sorted(window_order(report, [m for m in members if m.get("role") == "main"]),
                   key=lambda m: m["participant"] != current)
    calls = [m for m in members if m.get("role") != "main"] + native_advisors(report, members)
    owners = {ident: m for m in mains for ident in (m["participant"], m.get("attempt")) if ident}
    for row in worker_rows(report):
        if row["source"] == "council" and row["parent"] in owners:
            owner = owners[row["parent"]]
            calls.append({"role": "council", "parent": row["parent"], "state": row["state"],
                          "title": row["work"], "model": row["name"],
                          "_action": ("debate", (row["task"], owner["participant"]))})
    calls.sort(key=call_order)
    if attention_only:
        calls = [m for m in calls if m["state"] in ATTENTION_STATES or classify(m) == "review"]
    prefix, end, stem = ("├─ ", "└─ ", "│  ") if unicode else ("|- ", "`- ", "|  ")
    rows, items = [], []
    boxed = width >= 28 and height >= 16
    content_width = width - 4 if boxed else width

    def add(text, style=None, action=None, fold=False, divider=False):
        rows.append({"text": clipped(text, content_width), "style": style, "action": action,
                     "fold": fold, "divider": divider})
        if action:
            items.append(action)

    def phrase(call):
        return call_span(call, "→" if unicode else "->")

    def call_groups(attached, owner, root_stem):
        groups = [(role, [m for m in attached if m.get("role") == role and classify(m) == "live"])
                  for role in ("council", "advisor", "reviewer", "worker")]
        groups += [(kind, [m for m in attached if classify(m) == kind]) for kind, _ in CALL_GROUPS]
        groups = [(role, children) for role, children in groups if children]
        for group_index, (role, children) in enumerate(groups):
            last_group = group_index == len(groups) - 1
            group_title = dict(CALL_GROUPS).get(role)
            group_key = (role, owner)
            heading = group_title or ("COUNCIL" if role == "council" else role.upper() + "S")
            if group_title:
                mark = ("▾" if group_key in expanded else "▸") if unicode else ("v" if group_key in expanded else ">")
                unknown = sum(c["state"] == "presence unknown" for c in children)
                add(root_stem + (end if last_group else prefix) + "%s %s (%s)%s" % (
                    mark, heading, len(children), " / %s earlier unknown" % unknown if unknown else ""),
                    "review" if role == "review" else "dim", ("group", group_key))
                if group_key not in expanded:
                    continue
            else:
                add(root_stem + (end if last_group else prefix) + heading + " (%s)" % len(children),
                    "worker" if role == "worker" else "review")
            indent = root_stem + ("   " if last_group else stem)
            for child_index, child in enumerate(children):
                branch = end if child_index == len(children) - 1 else prefix
                model = model_name(child)
                label = child.get("title") or child["participant"]
                child_action = child.get("_action") or ("result", child["participant"])
                if child.get("_native"):
                    child_action = None
                    label += sep + "answer stays in the main transcript"
                add(indent + branch + "%s %s%s%s" % (activity(child["state"], frame, unicode), model, sep, phrase(child) + (sep + "patch awaits admission"
                        if child["state"] == "done" and child.get("participant") in listing(report.get("awaiting_admission")) else "")),
                    tone(child["state"]), child_action)
                for line in wrapped(label, max(1, content_width - display_width(indent) - 3), 2):
                    add(indent + "   " + line, None, child_action)

    roots = set(owners)
    for n, main in enumerate(mains):
        root_stem = "  "
        attached = [m for m in calls if m.get("parent") and
                    m["parent"] in {main["participant"], main.get("attempt")}]
        action = ("chat", main["participant"])
        folded = main["participant"] in collapsed
        toggle = ("▸" if folded else "▾") if unicode else (">" if folded else "v")
        state = "exited" if main["state"] == "done" else main["state"]
        said_state = state if state == "exited" else said(state)
        model = names.get(main["participant"]) or model_name(main)
        # The node carries one mark: the fold arrow, or its state where there is nothing to fold.
        label = "%s %s%s" % (toggle if attached else activity(state, frame, unicode),
                             "this window" + sep if main["participant"] == current else "", model)
        status = said_state + (sep + "%s calls" % len(attached) if folded else "")
        if folded:
            reviews = sum(classify(m) == "review" for m in attached)
            if reviews:
                status += sep + "Needs review (%s)" % reviews
        add(label if width < 60 else label + sep + status, "codex" if main.get("provider") == "codex" else "main",
            action, bool(attached))
        if width < 60:
            add("  " + status, tone(state), action)
        title = main.get("title")
        if title and title != "Native task not recorded":
            for line in wrapped(title, max(1, content_width - 7), 2):
                add(root_stem + "  " + line, None, action)
        for line in declared_status(room, main["participant"], max(1, content_width - 4)):
            add(root_stem + "  " + line, None, action)
        if not folded:
            call_groups(attached, main["participant"], root_stem)
    unlinked = [m for m in calls if m.get("parent") not in roots]
    if unlinked:
        add("Calls with no known main", "alert")
        call_groups(unlinked, "unlinked", "")
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
        verified = "%s of %s tasks verified" % (mapping(acceptance.get("counts")).get("verified", 0),
                                                acceptance.get("total", "?"))
        idle = plan_idle(report) or 0
        if idle >= 7:
            add(sep.join(["Old plan", "idle %s days" % idle, verified]), "dim")
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
    publications = mapping(room.get("publications"))
    if publications:
        footer.append("APP DELIVERY / " + " | ".join("%s: %s%s" % (
            target, mapping(receipt).get("status", "unknown"),
            " (unconfirmed)" if mapping(receipt).get("delivery_unknown") else "")
            for target, receipt in list(publications.items())[-3:]))
    def hints(help_open):
        chosen = next((m for m in members if member_action(m) == selected), None)
        return footer_hints(dict(navigation, keys_help=help_open), width, managed, unicode, chosen, len(mains), False)

    hint_rows = 0
    if not menu:
        shown_hints = hints(navigation.get("keys_help", False))
        footer += shown_hints
        hint_rows = len(shown_hints)
    if len(footer) > height - int(menu) - 4:
        footer = [clipped("1 Codex  2 Claude  ? More  q Quit", width)] if menu else hints(False)
        hint_rows = 0 if menu else len(footer)
    bar = None if menu or height < 8 else spawn_bar(width, navigation)
    if bar:
        # The start bar sits just above the key hints, which stay the last rows.
        footer.insert(len(footer) - hint_rows, bar[0])
    budget = max(0, height - int(menu) - len(footer))
    heading = "OMS / " + (clean(room.get("title") or room.get("id")) or "Work room")
    banner = [{"text": clipped(text, width), "style": style, "action": ("tab", "plan")}
              for text, style in goal_banner(report, width, unicode)] if height >= 10 else []
    head = banner + [{"text": clipped(heading, width), "style": "main"}]
    head += [{"text": clipped(alert, width), "style": "bad"} for alert in status_alerts(report)]
    if boxed and height >= 20:
        usage = usage_words(report, unicode)
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
            content.append(("Usage limits".ljust(left) + "  Model use, last 8 calls", "dim"))
            for index in range(max(len(metric_lines), len(labels))):
                metric = metric_lines[index] if index < len(metric_lines) else ""
                label = labels[index] if index < len(labels) else ""
                metric = clipped(metric, left)
                content.append((metric + " " * (left - display_width(metric)) + "  " + label, None))
        else:
            content = [(line, None) for line in metric_lines]
            content += [("Model use, last 8 calls", "dim")] + [(line, None) for line in labels]
        head.append({"text": box_edge("USAGE / W used, C left" if content_width < 40 else "USAGE", width, unicode), "style": "dim"})
        head += [{"text": box_row(line, width, unicode), "style": style} for line, style in content]
        head.append({"text": box_edge("", width, unicode, "bottom"), "style": "dim"})
    elif budget - len(head) >= 5:
        head += [{"text": clipped(line, width), "style": "dim"} for line in header_rows(report, width)]
    if budget - len(head) >= 5:
        unread = live_unread(report, members, settled)
        mailbox = "Messages: %s unread for live participants" % unread
        if not summary:
            mailbox += sep + "%s read" % room.get("received_count", 0) + sep + "%s answered" % room.get("answered_count", 0)
        head.append({"text": clipped(mailbox, width), "style": "alert" if unread else "dim"})
    # The viewport owns its borders, so scrolling never leaves an open card.
    if boxed and budget - len(head) < 3:
        head = head[:len(banner) + 1 + len(status_alerts(report))]
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
    if not menu:
        output += [""] * max(0, height - len(output) - len(footer))
    output += [PALETTE["dim"] + clipped(line, width) + "\033[0m" if color and
               line.startswith(("╭", "╰", "+", "OMS records")) else clipped(line, width)
               for line in footer]
    if bar:
        row = len(output) - hint_rows - 1
        output[row] = PALETTE["main" if bar[1] else "dim"] + output[row] + "\033[0m" if color else output[row]
        hits += [{"y": row + 1, "x1": x1, "x2": x2, "action": ("spawn", name)} for x1, x2, name in bar[1]]
    navigation.update(hits=hits, items=unique, offset=offset, body_rows=len(body), viewport=room_budget,
                      room_id=room.get("id"), positions=positions, geometry=geometry, surface="tree", bands=[])
    return "\n".join(output)


def render_detail(report, detail, width, height, navigation, managed=False):
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
    navigation.update(hits=[], items=[], offset=offset, viewport=budget, body_rows=len(body), bands=[])
    lines = heading[:max(0, height - 2)] + body[offset:offset + budget]
    lines += ["%s-%s / %s rows" % (offset + 1, min(len(body), offset + budget), len(body)),
              "Esc Back  PgUp/PgDn Scroll" + ("  F9 Chat/Board" if managed else "  q Quit")]
    return "\n".join(clipped(line, width) for line in lines[-height:])
