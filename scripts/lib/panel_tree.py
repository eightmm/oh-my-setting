"""Connected room tree and a frame-local map of navigation targets."""

from dashboard_projection import clean, display_width, listing, mapping
from panel_metrics import usage_line
from panel_view import (ATTENTION_STATES, PALETTE, activity, box_edge, box_row, clipped,
                        main_occupied, menu_rows, status_alerts, TERMINAL_STATES, tone, usage_words, worker_rows, wrapped)
from room_view import (action as member_action, call_order, past_calls, context_note, open_count, footer_hints, live_unread,
                       goal_banner, main_names, model_name, native_advisors, nodes, plan_idle, readable, said,
                       settler, spawn_bar, call_classifier, CALL_GROUPS, declared_status, main_state, main_words)


def render_tree(report, width, height, color=False, unicode=True, frame=None, menu=False,
                attention_only=False, main_attempt=None, navigation=None, summary=False, managed=False,
                previous=None):
    from panel_view import use_unicode
    use_unicode(unicode)
    interactive = navigation is not None
    navigation = navigation if interactive else {}
    navigation.update(tab_strip_visible=False, tab_body_visible=False, help_rows=max(4, height // 3))
    sep = " · " if unicode else " / "
    selected = navigation.get("selected")
    collapsed = navigation.get("collapsed", set())
    room = mapping(report.get("room"))
    if height <= int(menu):
        navigation.update(hits=[], items=[], offset=0, viewport=0, positions={}, room_id=room.get("id"))
        return ""
    members = nodes(report)
    navigation["main_occupied"] = bool(
        navigation.get("room_poll_unknown") or navigation.get("capacity_stale") or report.get("capacity_stale")
        or room.get("error") or mapping(report.get("collection")).get("ok") is not True
        or main_occupied(report, members))
    names = main_names(members)
    settled, _ = settler(report)
    classify = call_classifier(report)
    expanded = navigation.get("expanded_groups", set())
    current = next((m["participant"] for m in members if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)), None)
    # This window's main leads so its team stays on screen; earlier mains of the room follow.
    mains = sorted([m for m in members if m.get("role") == "main"], key=lambda m: m["participant"] != current)
    calls = [m for m in members if m.get("role") != "main"] + native_advisors(report, members)
    owners = {ident: m for m in mains for ident in (m["participant"], m.get("attempt")) if ident}
    for row in worker_rows(report):
        if row["source"] == "council" and row["parent"] in owners:
            owner = owners[row["parent"]]
            calls.append({"participant": row["task"], "role": "council", "parent": row["parent"],
                          "state": row["state"], "title": row["work"], "model": row["name"],
                          "_action": ("debate", (row["task"], owner["participant"]))})
    calls.sort(key=call_order)
    if attention_only:
        calls = [m for m in calls if classify(m) != "past" and
                 (m["state"] in ATTENTION_STATES or classify(m) == "review")]
    past = past_calls(calls, classify)
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
        from room_view import LIVE_STATES, elapsed, end_words
        if call.get("_native"):
            return "answered" if call.get("_finished") else "running"
        words = end_words(call, said(call["state"]))
        return " ".join(part for part in (words, elapsed(call.get("joined_at")) if call["state"] in LIVE_STATES else "") if part)

    def call_groups(attached, owner, root_stem):
        from room_view import clock_stamp
        groups = [(role, [m for m in attached if m.get("role") == role and classify(m) == "live"])
                  for role in ("council", "advisor", "reviewer", "researcher", "worker")]
        groups += [(kind, [m for m in attached if classify(m) == kind]) for kind, _ in CALL_GROUPS]
        groups = [(role, children) for role, children in groups if children]
        for group_index, (role, children) in enumerate(groups):
            last_group = group_index == len(groups) - 1
            group_title = dict(CALL_GROUPS).get(role)
            group_key = (role, owner)
            heading = group_title or ("COUNCIL" if role == "council" else role.upper() + "S")
            if group_title:
                mark = ("▾" if group_key in expanded else "▸") if unicode else ("v" if group_key in expanded else ">")
                add(root_stem + (end if last_group else prefix) + "%s %s (%s)" % (mark, heading, len(children)),
                    "review" if role == "review" else "dim", ("group", group_key))
                if group_key not in expanded:
                    continue
            else:
                add(root_stem + (end if last_group else prefix) + heading + " (%s)" % len(children),
                    "worker" if role in {"worker", "researcher"} else "review")
            indent = root_stem + ("   " if last_group else stem)
            for child_index, child in enumerate(children):
                branch = end if child_index == len(children) - 1 else prefix
                model = model_name(child)
                if child.get("role") == "researcher":
                    model = "Researcher " + model
                label = child.get("title") or child["participant"]
                child_action = child.get("_action") or ("result", child["participant"])
                if child.get("_native"):
                    child_action = None
                    label += sep + "answer stays in the main transcript"
                clock = " → ".join(filter(None, (clock_stamp(child.get("joined_at")), clock_stamp(child.get("ended"))))) if unicode else \
                    " -> ".join(filter(None, (clock_stamp(child.get("joined_at")), clock_stamp(child.get("ended")))))
                if child.get("_native"):
                    clock = child.get("_finished") or child["_native"]
                lead = indent + branch + activity(child["state"], frame, unicode) + " " + phrase(child)
                text = lead + sep + model
                if display_width(text + sep + clock) <= content_width and clock:
                    text += sep + clock
                if display_width(text) > content_width:
                    text = lead + sep + clipped(model, max(0, content_width - display_width(lead + sep)))
                add(text, tone(child["state"]), child_action)
                if child["state"] == "done" and child.get("participant") in listing(report.get("awaiting_admission")):
                    add(indent + "   patch awaits admission", "alert", child_action)
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
        state = main_state(main)
        said_state = main_words(main, unicode)
        model = names.get(main["participant"]) or model_name(main)
        # The node carries one mark: the fold arrow, or its state where there is nothing to fold.
        label = "%s %s%s" % (toggle if attached else activity(state, frame, unicode),
                             "this window" + sep if main["participant"] == current else "", model)
        status = said_state + (sep + "%s calls" % len(attached) if folded else "")
        if open_count(room, main["participant"]):
            status += sep + "? %s open" % open_count(room, main["participant"])
        context = context_note(report, main)
        if context:
            status += sep + "ctx %s%% used" % context[0]
        add(label if width < 60 else label + sep + status, "dim" if state == "idle" else "codex" if main.get("provider") == "codex" else "main",
            action, bool(attached))
        if width < 60:
            add("  " + status, "dim" if state == "idle" else tone(state), action)
        title = main.get("title")
        if title and title != "Native task not recorded":
            for line in wrapped(title, max(1, content_width - 7), 3 if height >= 40 else 2):
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
        if menu and not navigation["main_occupied"]:
            add(menu_rows(width, 1, unicode, managed=managed,
                          occupied=navigation["main_occupied"], installed=navigation.get("installed"))[0], "main")
    acceptance = mapping(report.get("acceptance"))
    if acceptance.get("available"):
        verified = "%s of %s tasks verified" % (mapping(acceptance.get("counts")).get("verified", 0),
                                                acceptance.get("total", "?"))
        idle = plan_idle(report) or 0
        if idle >= 7:
            add(sep.join(["Old plan", "idle %s days" % idle, verified]), "dim")
    from room_view import clock_stamp, result_handoff
    messages = [m for m in listing(room.get("messages")) if not result_handoff(room, m)]
    for message in messages[-2:]:
        sender = names.get(message.get("sender")) or next((model_name(m) for m in members
                 if m["participant"] == message.get("sender")), clean(message.get("sender")) or "unknown")
        add(clock_stamp(message.get("ts"))[-5:] + " " + sender + sep + (readable(message.get("text"), " ") or ""), "dim")
    tasks = listing(room.get("repo_tasks"))
    task_start = len(rows)
    if tasks:
        add("Repository tasks", "worker", divider=True)
        for task in tasks:
            action = ("task", task.get("id"))
            add("%s%s%s" % ("in review" if task.get("state") == "review" else said(task.get("state")), sep,
                             task.get("title") or task.get("id")),
                tone(task.get("state")), action)
    footer = menu_rows(width, 1, unicode, navigation.get("menu_help", False), managed,
                       occupied=navigation["main_occupied"], installed=navigation.get("installed")) if menu else []
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    def hints(help_open):
        chosen = next((m for m in members if member_action(m) == selected), None)
        return footer_hints(dict(navigation, keys_help=help_open), width, managed, unicode, chosen, False)

    hint_rows = 0
    if not menu:
        chosen = next((m for m in members if member_action(m) == selected), None)
        shown_hints = footer_hints(dict(navigation, tab_strip_visible=True, tab_body_visible=True),
                                   width, managed, unicode, chosen, True)
        hint_rows = max(4, len(shown_hints)) if navigation.get("keys_help") else len(shown_hints)
        footer += shown_hints + [""] * (hint_rows - len(shown_hints))
    if len(footer) > height - int(menu) - 4:
        footer = menu_rows(width, 1, unicode, managed=managed, occupied=navigation["main_occupied"],
                           installed=navigation.get("installed")) if menu else hints(False)
        hint_rows = 0 if menu else len(footer)
    if menu and previous is not None:
        from panel_view import message_rows
        last_rows = (message_rows("Last: " + previous, width)
                     if "\n" in previous or "oms panel --release-main " in previous else ["Last: " + previous])
        # Reserve status before optional body/dock rows; recovery commands must stay copyable.
        footer = footer[:max(0, height - 1 - len(last_rows))] + last_rows[:height - 1]
    bar = None if menu or height < 8 else spawn_bar(width, navigation)
    if bar:
        # The start bar sits just above the key hints, which stay the last rows.
        footer.insert(len(footer) - hint_rows, bar[0])
    budget = max(0, height - int(menu) - len(footer))
    import os
    from panel_view import attention_items, needs_strip
    # One ranked queue per frame feeds the strip and the Attention tab, also across the auto-fold re-render.
    ranked = navigation.pop("_attention", None)
    ranked = attention_items(report, unicode) if ranked is None else ranked
    repo = mapping(report.get("repo"))
    heading = "OMS" + sep + (os.path.basename(clean(repo.get("name")) or str(repo.get("path") or "").rstrip("/\\")) or "repository")
    alerts = status_alerts(report)
    banner = goal_banner(report, width, unicode, True)
    head = [{"text": clipped(heading, width), "style": "main"},
            {"text": clipped(usage_line(report, width, unicode), width), "style": "dim"}]
    head += [{"text": clipped(text, width), "style": style, "action": ("tab", "plan")} for text, style in banner]
    head += [{"text": clipped(alert, width), "style": "bad"} for alert in alerts]
    head += needs_strip(report, width, unicode, ranked=ranked)
    others = listing(room.get("other_rooms"))
    if height >= 20 and (others or room.get("other_rooms_incomplete")):
        head.append({"text": clipped("OTHER ROOMS / " + " | ".join("%s %s live mains" % (
            clean(r.get("id"), 24), r.get("mains")) for r in others[:3]) +
            (" / scan incomplete" if room.get("other_rooms_incomplete") else ""), width), "style": "alert"})
    pinned = len(head)
    labels = usage_words(report, unicode)
    if boxed and height >= 20 and labels and budget - len(head) >= 5:
        if summary:
            labels = labels[:2] + (["+%s models / expand for more" % (len(labels) - 2)] if len(labels) > 2 else [])
        maximum = max(2, budget - len(head) - 9)
        if len(labels) > maximum:
            labels = labels[:maximum - 1] + ["+%s models / expand for more" % (len(labels) - maximum + 1)]
        content = [("Model use, last 8 calls", "dim")] + [(line, None) for line in labels]
        head.append({"text": box_edge("USAGE", width, unicode), "style": "dim"})
        head += [{"text": box_row(line, width, unicode), "style": style} for line, style in content]
        head.append({"text": box_edge("", width, unicode, "bottom"), "style": "dim"})
    # The viewport owns its borders, so scrolling never leaves an open card.
    if boxed and budget - len(head) < 3:
        head = head[:pinned]
    head = head[:budget]
    from room_view import tab_box
    main_rows = {}
    for i, row in enumerate(rows):
        if row.get("action") and row["action"][0] == "chat":
            main_rows.setdefault(row["action"], i)
    activity_floor = (max(main_rows.values()) + 1 if main_rows else 1) + int(bool(tasks))
    # A chosen preview or an open Past or Attention list is what the person asked to read, so the dock may push later mains down.
    reading = navigation.get("preview") or navigation.get("tab") == "past" and past or navigation.get("tab") == "needs" and ranked
    if reading or interactive and height >= 32:
        activity_floor = min(activity_floor, 3 + int(bool(tasks)))
    activity_floor += 2 if boxed else 0
    dock_rows = (14 if budget - len(head) >= 14 + activity_floor else int(budget > len(head))) if width >= 20 else 0
    if dock_rows == 1 and reading and budget - len(head) - activity_floor >= 3:
        dock_rows = min(14, budget - len(head) - activity_floor)
    navigation.update(tab_strip_visible=bool(dock_rows), tab_body_visible=dock_rows >= 3)
    activity_head = boxed and budget - len(head) - dock_rows >= 2
    boxed = boxed and budget - len(head) - dock_rows >= 5
    room_budget = max(0, budget - len(head) - dock_rows - (2 if boxed else int(activity_head)))
    body = rows
    # Spend spare tree rows on a third title row while the nested group renderer stays shared.
    if height >= 40 and room_budget > len(body):
        for child in calls:
            if child.get("_native"):
                continue
            target = child.get("_action") or ("result", child["participant"])
            first = next((i for i, row in enumerate(body) if row.get("action") == target), None)
            if first is None or first + 1 >= len(body) or body[first + 1].get("action") != target:
                continue
            prefix_text = body[first + 1]["text"][:len(body[first + 1]["text"]) - len(body[first + 1]["text"].lstrip())]
            title_rows = wrapped(child.get("title") or child["participant"], max(1, content_width - len(prefix_text)), 3)
            old_count = 0
            while first + 1 + old_count < len(body) and body[first + 1 + old_count].get("action") == target:
                old_count += 1
            if len(title_rows) > old_count and len(title_rows) - old_count <= room_budget - len(body):
                body[first + 1:first + 1 + old_count] = [{"text": prefix_text + text, "style": None,
                    "action": target} for text in title_rows]
    unique = list(dict.fromkeys([row["action"] for row in head if row.get("action") and row["action"][0] != "tab"] + items))
    if len(body) > room_budget > 0 and not navigation.get("_auto_fold"):
        # Too short for everything: earlier mains other than the selected one drop to their status line, kept only if that puts every main on screen and leaves a call to select.
        keep = {current} | {m["participant"] for m in mains if selected and (
            selected == ("chat", m["participant"]) or any(
                member_action(c) == selected for c in calls
                if c.get("parent") in {m["participant"], m.get("attempt")}))}
        keep = keep if keep - {None} else {m["participant"] for m in mains[:1]}
        extra = {m["participant"] for m in mains} - keep
        inner = dict(navigation, collapsed=collapsed | extra, _auto_fold=True, _attention=ranked)
        text = render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, inner, summary, managed, previous=previous)
        reachable = any(i[0] == "result" for i in inner["items"]) or not any(i[0] == "result" for i in items)
        if reachable and all(inner["positions"].get(("chat", m["participant"]), 0) < inner["viewport"] for m in mains):
            navigation.update({k: v for k, v in inner.items() if k not in ("collapsed", "_auto_fold")})
            navigation["items"] = list(dict.fromkeys(unique + navigation["items"]))
            return text
    # Keep the repository task tail reachable while the longer room tree scrolls.
    task_rows = body[task_start:]
    first_call = next((i + 1 for i, row in enumerate(body) if row.get("action") and
                       row["action"][0] in {"result", "debate"}), 0)
    body_floor = max(max(main_rows.values(), default=-1) + 1, first_call)
    task_tail = task_rows if tasks and len(task_rows) + body_floor <= room_budget else task_rows[-1:] if tasks and room_budget else []
    if task_tail and len(task_tail) == len(task_rows):
        body = body[:task_start]
    elif task_tail:
        body = body[:-1]
    else:
        task_tail = []
    body_budget = max(0, room_budget - len(task_tail))
    positions = {}
    for index, row in enumerate(body):
        if row.get("action"):
            positions.setdefault(row["action"], index)
    for index, row in enumerate(task_tail):
        if row.get("action"):
            positions.setdefault(row["action"], len(body) + index)
    scroll_row = len(body) > body_budget and body_budget > 0
    if scroll_row and task_tail:
        # The task tail already marks the bottom of this viewport; keep the
        # last room row instead of spending another row on a scroll counter.
        scroll_row = False
    elif scroll_row:
        body_budget -= 1
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - body_budget))
    geometry = (width, height, menu, summary)
    if body_budget and selected in positions and positions[selected] < len(body) and (
            navigation.get("tree_selected") != selected or navigation.get("geometry") != geometry or
            navigation.get("viewport") != body_budget):
        position = positions[selected]
        if (selected[0] == "group" and selected[1] in expanded and body_budget > 1
                and position + 1 < len(body) and body[position + 1].get("action")
                and body[position + 1]["action"][0] in {"result", "debate"}
                and position + 1 >= offset + body_budget):
            offset = position
        elif position < offset:
            offset = position
        elif position >= offset + body_budget:
            offset = position - body_budget + 1
    shown = list(head)
    if activity_head:
        heading = "Activity%s%s main%s%s%s call%s%s" % (
            sep, len(mains), "" if len(mains) == 1 else "s", sep, len(calls),
            "" if len(calls) == 1 else "s", sep + "attention" if attention_only else "")
        shown.append({"text": box_edge(heading, width, unicode) if boxed else clipped(heading, width),
                      "style": "main"})
    shown += body[offset:offset + body_budget]
    if scroll_row:
        shown.append({"text": clipped("%s-%s / %s rows / %s" % (offset + 1, min(len(body), offset + body_budget), len(body),
                      "j/k to scroll" if menu else "scroll for more"), content_width),
                      "style": "dim", "action": None})
    shown += task_tail
    if boxed:
        shown.append({"text": box_edge("", width, unicode, "bottom"), "style": "main"})
    output, hits = [], []
    previous_action = None
    for y, row in enumerate(shown):
        value = row["text"]
        style = row.get("style")
        inner = boxed and len(head) < y < len(shown) - 1
        if not color and row.get("action") and row["action"] == selected and row["action"] != previous_action:
            span = content_width if inner else width
            value = clipped(value, max(0, span - 2)) + " <" if span >= 2 else "<"
        previous_action = row.get("action")
        if inner:
            value = box_edge(value, width, unicode, "divider") if row.get("divider") else box_row(value, width, unicode)
        if color and row.get("action") and row["action"] == selected:
            value = PALETTE[style or "head"] + "\033[7m" + value + "\033[0m"
        elif color and style:
            value = PALETTE[style] + value + "\033[0m"
        output.append(value)
        if row.get("action"):
            hits.append({"y": y + 1, "x1": 1, "x2": width, "action": row["action"], "fold": row.get("fold", False)})
    output += [""] * max(0, budget - dock_rows - len(output))
    dock_top = len(output)
    everyone = {m["participant"]: [c for c in calls if c.get("parent") in
                {m["participant"], m.get("attempt")}] for m in mains}
    dock_lines, dock_hits = tab_box(report, width, dock_rows, navigation, color, unicode,
                                    main_attempt=main_attempt, everyone=everyone, past=past, attention=ranked)
    if dock_rows == 1:
        dock_lines = [line.replace("╭", "[").replace("╮", "]") for line in dock_lines]
    output += dock_lines
    hits += [dict(hit, y=hit["y"] + dock_top) for hit in dock_hits]
    bands = []
    if navigation.get("box"):
        navigation["box"] = {**navigation["box"], "y1": navigation["box"]["y1"] + dock_top,
                             "y2": navigation["box"]["y2"] + dock_top}
        bands.append(dict(navigation["box"], name="detail", x1=1, x2=width, step=3))
    # Refit hints after the dock has supplied its actual visibility and preview state.
    if not menu:
        chosen = next((m for m in members if member_action(m) == selected), None)
        fitted = footer_hints(navigation, width, managed, unicode, chosen, True)
        footer[-hint_rows:] = fitted[:hint_rows] + [""] * max(0, hint_rows - len(fitted))
        output += [""] * max(0, height - len(output) - len(footer))
    output += [PALETTE["dim"] + clipped(line, width) + "\033[0m" if color and
               line.startswith(("╭", "╰", "+", "OMS records")) else clipped(line, width)
               for line in footer]
    if bar:
        row = len(output) - hint_rows - 1
        output[row] = PALETTE["main" if bar[1] else "dim"] + output[row] + "\033[0m" if color else output[row]
        hits += [{"y": row + 1, "x1": x1, "x2": x2, "action": ("spawn", name)} for x1, x2, name in bar[1]]
    for hit in dock_hits:
        target = hit["action"]
        if target[0] in {"result", "debate"} and target not in unique:
            before_task = next((i for i, action in enumerate(unique) if action[0] == "task"), len(unique))
            unique.insert(before_task, target)
    navigation.update(hits=hits, items=unique, offset=offset, body_rows=len(body), viewport=body_budget,
                      room_id=room.get("id"), positions=positions, geometry=geometry, surface="tree", bands=bands,
                      tree_selected=selected)
    # A Past- or Attention-list choice scrolled off its page stays chosen; only the board's own rows are reaped.
    tab = navigation.get("tab")
    kept = (tab in {"past", "needs"} and tab in navigation.get("visible_tabs", [])
            and selected in navigation.get(tab + "_ids", []))
    if selected not in unique and not kept:
        navigation["selected"] = None
        navigation.pop("selected_row", None)
        navigation.pop("row_ids", None)
    return "\n".join(output)


def render_detail(report, detail, width, height, navigation, managed=False):
    from panel_view import _result_blocks, _result_structured
    heading = [clipped("OMS / " + detail["title"], width), clipped(usage_line(report, width), width)]
    if "report" in detail:
        from panel_view import render_results
        # The result reader already wrapped and sanitized every line at this width.
        body = render_results(detail["report"], width).split("\n")
    else:
        body = []
        for line, kind in _result_blocks(detail["text"]):
            body.extend(_result_structured(line, width) if kind != "prose" else
                        wrapped(line, width, max(1, len(line)), max_chars=max(400, len(line))))
    budget = max(0, height - len(heading) - 2)
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - budget))
    navigation.update(hits=[], items=[], offset=offset, viewport=budget, body_rows=len(body), bands=[])
    lines = heading[:max(0, height - 2)] + body[offset:offset + budget]
    lines += [clipped("%s-%s / %s rows" % (offset + 1, min(len(body), offset + budget), len(body)), width),
              clipped("Esc Back  PgUp/PgDn Scroll" + ("  F9 Chat/Board" if managed else "  q Quit"), width)]
    return "\n".join(lines[-height:])
