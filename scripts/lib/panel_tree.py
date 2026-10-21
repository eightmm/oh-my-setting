"""Connected room tree and a frame-local map of navigation targets."""

import re

from dashboard_projection import clean, display_width, listing, mapping
from panel_metrics import usage_line
from panel_view import (ATTENTION_STATES, LIVE_STATES, PALETTE, activity, box_edge, box_row, clipped,
                        _repo_root, main_occupied, menu_rows, status_alerts, TERMINAL_STATES, worker_rows, wrapped,
                        role_style, call_identity, status_tag, glyph, paint, color_count, light_theme, age_label,
                        JUDGE_ROLES, WORKER_ROLES, paint_card_edges, judge_band, band_style, role_legend,
                        judgment_tag, judgment_tone, call_ending, ending_tag, call_tokens)
from room_view import (action as member_action, call_order, past_calls, context_note, open_count, footer_hints, live_unread,
                       goal_banner, native_advisors, nodes, plan_idle,
                       settler, spawn_bar, call_classifier, CALL_GROUPS, declared_status, main_state, main_words)


def render_tree(report, width, height, color=False, unicode=True, frame=None, menu=False,
                attention_only=False, main_attempt=None, navigation=None, summary=False, managed=False,
                previous=None):
    from panel_view import use_unicode
    use_unicode(unicode)
    color = bool(paint("x", "text", color=color) != "x")
    interactive = navigation is not None
    navigation = navigation if interactive else {}
    navigation.update(tab_strip_visible=False, tab_body_visible=False, help_rows=max(4, height // 3))
    cached_activity = navigation.get("activity_snapshot")
    if not isinstance(cached_activity, dict):
        cached_activity = mapping(mapping(navigation.get("preview")).get("report")).get("activity_snapshot")
    root = _repo_root(report)
    if isinstance(cached_activity, dict) and cached_activity.get("repo") == root:
        navigation["activity_snapshot"] = cached_activity
    else:
        navigation["activity_snapshot"] = {"repo": root, "calls": {}}
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
    settled, _ = settler(report)
    classify = call_classifier(report)
    expanded = navigation.get("expanded_groups", set())
    current = next((m["participant"] for m in members if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)), None)
    # One main owns the root. Earlier mains remain in room history and lineage.
    all_mains = [m for m in members if m.get("role") == "main"]
    live_mains = [m for m in all_mains if m["state"] not in TERMINAL_STATES]
    # Without this window's own main, a main with live attempt evidence leads one held or of unknown presence;
    # once every main has ended, the window's own or the first one keeps the root, as on the board.
    live_main = next((m for m in live_mains if m["participant"] == current), None) or next(
        (m for m in live_mains if m["state"] in LIVE_STATES), next(iter(live_mains), None)) or next(
        (m for m in all_mains if m["participant"] == current), next(iter(all_mains), None))
    mains = [live_main] if live_main else []
    window_mains = {live_main["participant"], live_main.get("attempt")} if live_main else set()
    # Claude Code's own agents belong to the main this window shows, never to another main of the room.
    calls = [m for m in members if m.get("role") != "main"] + [
        m for m in native_advisors(report, members) if m.get("_kind") not in {"agent", "workflow"} or m["parent"] in window_mains]
    owners = {ident: m for m in all_mains for ident in (m["participant"], m.get("attempt")) if ident}
    for row in worker_rows(report):
        if row["source"] == "council" and row["parent"] in owners:
            owner = owners[row["parent"]]
            calls.append({"participant": row["task"], "role": "council", "parent": row["parent"],
                          "state": row["state"], "title": row["work"], "model": row["name"],
                          "_action": ("debate", (row["task"], owner["participant"]))})
    attempts = listing(mapping(report.get("attempts")).get("active_recent")) + listing(mapping(report.get("attempts")).get("recent"))

    def purpose(call):
        meta = next((mapping(a.get("panel")) for a in attempts if a.get("attempt_id") == call.get("attempt")
                     and a.get("attempt_id")), {})
        return meta.get("purpose") or call.get("purpose")

    def visual_role(call):
        return purpose(call) if purpose(call) in JUDGE_ROLES | WORKER_ROLES else call.get("role")

    calls.sort(key=lambda call: call["state"] not in LIVE_STATES)
    def token_label(call):
        if call.get("_native") or not call.get("attempt"):
            return ""
        value = call_tokens(report, call.get("attempt"), call["state"], True)
        return "" if value in {"tok pending", "tok n/a"} else value

    drawn_calls = calls + mains
    age_width = max([3] + [display_width(main_words(call, unicode) if call.get("role") == "main"
                                           else age_label(call.get("joined_at")) or "unknown")
                           for call in drawn_calls if not call.get("_native")])
    tag_width = max([0] + [display_width(judgment_tag(dict(call, purpose=purpose(call)), report, unicode,
                                                        ending_tag(dict(call, state=main_state(call) if call.get("role") == "main" else call["state"]), unicode)))
                            for call in drawn_calls if not call.get("_native")])
    token_width = max([0] + [display_width(token_label(call)) for call in calls if token_label(call)])
    if attention_only:
        calls = [m for m in calls if classify(m) != "past" and
                 (m["state"] in ATTENTION_STATES or classify(m) == "review")]
    past = past_calls(calls, classify)
    prefix, end, stem = ("├╌╌ ", "└╌╌ ", "│   ") if unicode else ("+-  ", "`-  ", "|   ")
    top = "┌╌╌ " if unicode else ",-  "
    rows, items = [], []
    boxed = width >= 28 and height >= 16
    content_width = width - 4 if boxed else width

    def add(text, style=None, action=None, fold=False, divider=False):
        rows.append({"text": clipped(text, content_width), "style": style, "action": action,
                     "fold": fold, "divider": divider})
        if action:
            items.append(action)

    def call_row(call, connector="", action=None, fold=False, extra=""):
        state = main_state(call) if call.get("role") == "main" else call["state"]
        tag = judgment_tag(dict(call, purpose=purpose(call)), report, unicode,
                           ending_tag(dict(call, state=state), unicode))
        tag_tone = judgment_tone(tag, call_ending(call) or state)
        age = main_words(call, unicode) if call.get("role") == "main" else age_label(call.get("joined_at")) or "unknown"
        tokens = token_label(call)
        label = call.get("title") or (None if call.get("role") == "main" else call["participant"])
        if not extra and call.get("state") in LIVE_STATES:
            latest = mapping(mapping(navigation.get("activity_snapshot")).get("calls")).get(
                call.get("participant")) or []
            if latest:
                extra = latest[-1].get("text", "")
        identity = call_identity(call.get("role"), call.get("model"), label, alias=call.get("alias"),
                                 purpose=purpose(call))
        if extra:
            identity += sep + extra
        if call.get("role") == "main":
            opened = open_count(room, call["participant"])
            if opened:
                identity += sep + "? %s open" % opened
            context = context_note(report, call)
            if context:
                identity += sep + "ctx %s%% used" % context[0]
        available = max(0, content_width - 2 - display_width(connector))
        # Fit identity separately from the aligned state columns.
        age = main_words(call, unicode) if call.get("role") == "main" else age_label(call.get("joined_at")) or "unknown"
        row_token_width = token_width
        fixed = tag_width + 2 + age_width + (2 + row_token_width if row_token_width else 0)
        if row_token_width and available < fixed + 2:
            row_token_width = 0
        tag_pad = " " * max(0, tag_width - display_width(tag))
        age_pad = " " * max(0, age_width - display_width(age))
        token_pad = " " * max(0, row_token_width - display_width(tokens)) if row_token_width else ""
        tail = tag_pad + tag + "  " + ((token_pad + tokens + "  ") if row_token_width else "") + age_pad + age
        tail_width = display_width(tail)
        tiny_tail = False
        if tail_width > available:
            # Preserve tag and age until even those columns cannot fit.
            if available >= display_width(tag) + 2 + display_width(age):
                tail, tag_pad = tag + "  " + age, ""
            else:
                tail = clipped(tag + "  " + age, available)
                tiny_tail = True
            tail_width = display_width(tail)
        room_for_identity = max(0, available - tail_width - 2)
        identity = clipped(identity, room_for_identity)
        gutter = glyph("selected", unicode) + " " if action and selected == action else "  "
        padding = " " * max(2, available - display_width(identity) - tail_width)
        text = gutter + connector + identity + padding + tail
        style = band_style(call.get("role"), purpose(call))
        add(text, None, action, fold)
        rows[-1]["spans"] = [(gutter, "text"), (connector, style), (identity, style), (padding, "text")]
        if tiny_tail:
            rows[-1]["spans"].append((tail, "metadata"))
        else:
            rows[-1]["spans"].extend([(tag_pad, "metadata"), (tag, tag_tone),
                                      (tail[len(tag_pad) + len(tag):], "metadata")])
        rows[-1]["identity"] = action == selected
        if call.get("role") == "main" and action != selected:
            for note in declared_status(room, call["participant"], max(1, content_width - 6))[:1]:
                add("    " + note, "dim")
        # Native advisors have no retained result to open; their card still
        # explains where the answer lives when no selectable action exists.
        if (call.get("_native") or action and action == selected) and content_width >= 10:
            lead = ("  " + connector[:4] + " " * max(0, display_width(connector) - 4)
                    if width >= 40 else "")
            card_width = max(4, content_width - display_width(lead))
            rows.pop()
            add(lead + box_edge(call_identity(call.get("role"), call.get("model"), alias=call.get("alias"),
                                              purpose=purpose(call)), card_width, unicode, dashed=True), style, action, fold)
            rows[-1]["card_role"] = style
            add(lead + box_row(tag + "  " + age, card_width, unicode, dashed=True), None, action)
            rows[-1]["tag_span"] = (tag, tag_tone)
            rows[-1]["card_role"] = style
            for line in wrapped(label or "No task recorded", max(1, card_width - 4), 2):
                add(lead + box_row(line, card_width, unicode, dashed=True), None, action)
                rows[-1]["card_role"] = style
            notes = declared_status(room, call["participant"], max(1, card_width - 4)) if call.get("role") == "main" else []
            context = context_note(report, call) if call.get("role") == "main" else None
            if context:
                notes.append("ctx %s%% used" % context[0])
            if extra:
                notes.append(extra)
            if call.get("tokens") and not call.get("_native"):
                # The selected card replaces the row, so it keeps the row's token count.
                notes += wrapped(call["tokens"], max(1, card_width - 4), 2)
            if call.get("_kind") in {"agent", "workflow"}:
                notes += wrapped(call.get("tokens") or "", max(1, card_width - 4), 2)
                notes.append("Claude Code's own %s - not a panel worker: no admission, review or finalize" % call["_kind"])
            elif call.get("_native"):
                notes.append("answer stays in the main transcript")
            for note in notes:
                add(lead + box_row(note, card_width, unicode, dashed=True), None, action)
                rows[-1]["card_role"] = style
            add(lead + box_edge("", card_width, unicode, "bottom", dashed=True), style, action)

    above_group_count = {}

    def call_groups(attached, owner, band=None):
        roles = list(dict.fromkeys(visual_role(m) for m in attached))
        groups = [(role, [m for m in attached if visual_role(m) == role and classify(m) == "live"])
                  for role in roles]
        groups += [(kind, [m for m in attached if classify(m) == kind]) for kind, _ in CALL_GROUPS]
        groups = [(role, children) for role, children in groups if children]
        for group_index, (role, children) in enumerate(groups):
            last_group = group_index == len(groups) - 1
            group_title = dict(CALL_GROUPS).get(role)
            group_key = (role, owner)
            # Keep existing group keys unless two judgment bands would share one fold control.
            if band == "counsel" and group_title and any(
                    judge_band(m.get("role"), purpose(m)) == "gate" and classify(m) == role
                    and m.get("parent") in {owner, owners.get(owner, {}).get("attempt")} for m in calls):
                group_key = ("counsel:" + role, owner)
            heading = group_title or ("COUNCIL" if role == "council" else role.upper() + "S")
            if band and group_index == 0:
                heading = glyph(band, unicode) + " " + band.title() + sep + heading
            above_count = above_group_count.get(owner, 0)
            root_branch = (top if above_count == 0 else prefix) if band else (end if last_group else prefix)
            if band:
                above_group_count[owner] = above_count + 1
            continues = bool(band) or not last_group
            child_stem = stem if continues else "    "
            group_style = (("judge" if band == "gate" else "counsel") if band else
                           "review" if role == "review" else "dim") if group_title else band_style(role)
            if group_title:
                mark = ("▾" if group_key in expanded else "▸") if unicode else ("v" if group_key in expanded else ">")
                heading_action = ("group", group_key)
                heading_text = "%s %s (%s)" % (mark, heading, len(children))
                heading_gutter = glyph("selected", unicode) + " " if selected == heading_action else "  "
                add(heading_gutter + root_branch + heading_text, None, heading_action)
                rows[-1]["spans"] = [(heading_gutter, "text"), (root_branch, group_style),
                                     (heading_text, group_style)]
                if group_key not in expanded:
                    if height >= 16:
                        continue
                    # A short viewport still exposes a call directly, alongside the group control.
                    children = children[:1]
            else:
                heading_gutter = "  "
                heading_text = heading + " (%s)" % len(children)
                add(heading_gutter + root_branch + heading_text, None)
                rows[-1]["spans"] = [(heading_gutter, "text"), (root_branch, group_style),
                                     (heading_text, group_style)]
            for child_index, child in enumerate(children):
                branch = end if child_index == len(children) - 1 else prefix
                child_action = child.get("_action") or ("result", child["participant"])
                if child.get("_native"):
                    child_action = None
                call_row(child, child_stem + branch, child_action,
                         extra="patch awaits admission" if child["state"] == "done" and child.get("participant") in listing(report.get("awaiting_admission")) else "")

    roots = {ident for m in mains for ident in (m["participant"], m.get("attempt")) if ident}
    for main in mains:
        attached = [m for m in calls if m.get("parent") and
                    m["parent"] in {main["participant"], main.get("attempt")}]
        action = ("chat", main["participant"])
        folded = main["participant"] in collapsed
        if not folded:
            for band in ("gate", "counsel"):
                call_groups([m for m in attached if judge_band(m.get("role"), purpose(m)) == band],
                            main["participant"], band)
        call_row(main, action=action, fold=bool(attached), extra="%s calls" % len(attached) if folded else "")
        if not folded:
            call_groups([m for m in attached if role_style(m.get("role"), purpose(m)) != "judge"], main["participant"])
    unlinked = [m for m in calls if m.get("parent") not in roots]
    if unlinked:
        add("Calls with no known main", "alert")
        call_groups(unlinked, "unlinked")
    if interactive and height < 16 and not attention_only and calls and not any(
            item[0] in {"result", "debate"} for item in items):
        hidden = [c for c in calls if not c.get("_native")]
        if hidden:
            add("  " + glyph("ellipsis", unicode) + " %s more calls / Enter Detail" % len(hidden),
                "dim", member_action(hidden[0]))
    if height < 16:
        # Compact boards spend their activity rows on identities, rather than group headings or expanded cards.
        compact_rows, seen = [], set()
        for row in rows:
            target = row.get("action")
            if target and target[0] in {"chat", "result", "debate"} and target not in seen:
                compact_rows.append(row)
                seen.add(target)
        if compact_rows:
            first_main_row = next((r for r in compact_rows if r["action"][0] == "chat"), None)
            first_call_row = next((r for r in compact_rows if r["action"][0] in {"result", "debate"}), None)
            leading = [r for r in compact_rows if r is first_main_row or r is first_call_row]
            rows = leading + [r for r in compact_rows if r not in leading]
            items = [r["action"] for r in rows]
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
    tasks = listing(room.get("repo_tasks"))
    task_start = len(rows)
    if tasks:
        counts = {}
        for task in tasks:
            state = task.get("state") if isinstance(task.get("state"), str) else "unknown"
            counts[state] = counts.get(state, 0) + 1
        state_order = ("running", "review", "ready", "claimed", "landing",
                       "done", "blocked", "cancelled")
        task_summary = ["Plan"]
        for state in state_order:
            if counts.get(state):
                label = "in review" if state == "review" else state
                task_summary.append("%s %s" % (counts.pop(state), label))
        task_summary += ["%s %s" % (count, state) for state, count in sorted(counts.items())]
        task_summary.append("Plan tab")
        # The selected marker is prepended after clipping, so reserve its two columns here.
        summary_width = max(0, content_width - 2)
        while len(task_summary) > 2 and display_width(sep.join(task_summary)) > summary_width:
            task_summary.pop(-2)
        add(sep.join(task_summary), "dim", ("tab", "plan"))
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
    if height >= 20:
        legend_spans, room_left = [], width
        for i, (text, style) in enumerate(role_legend(unicode)):
            part = clipped(text + ("  " if i < 3 else ""), room_left)
            legend_spans.append((part, style))
            room_left -= display_width(part)
        head.append({"text": "".join(part for part, _ in legend_spans), "spans": legend_spans})
    head += [{"text": clipped(text, width), "style": style, "action": ("tab", "plan")} for text, style in banner]
    head += [{"text": clipped(alert, width), "style": "bad"} for alert in alerts]
    head += needs_strip(report, width, unicode, ranked=ranked)
    pinned = len(head)
    # The viewport owns its borders, so scrolling never leaves an open card.
    if boxed and budget - len(head) < 3:
        head = head[:pinned]
    head = head[:budget]
    from room_view import tab_box, dock_body_rows
    main_rows = {}
    for i, row in enumerate(rows):
        if row.get("action") and row["action"][0] == "chat":
            main_rows.setdefault(row["action"], i)
    first_call = next((i + 1 for i, row in enumerate(rows) if row.get("action") and
                       row["action"][0] in {"result", "debate"}), 0)
    activity_floor = max(max(main_rows.values(), default=-1) + 1, first_call, 1) + int(bool(tasks))
    # A chosen preview or an open Past or Attention list is what the person asked to read, so the dock may push later mains down.
    reading = navigation.get("preview") or navigation.get("tab") == "past" and past or navigation.get("tab") == "needs" and ranked
    if reading or interactive and height >= 32:
        activity_floor = min(activity_floor, 3 + int(bool(tasks)))
    activity_floor += 2 if boxed else 0
    detail_body = dock_body_rows(height)
    reader_target = navigation.get("reader_target") if navigation.get("reader_open") else None
    main_action = next((r["action"] for r in rows if r.get("action") and r["action"][0] == "chat"), None)
    target = mapping(navigation.get("preview")).get("target") or selected or main_action
    if navigation.get("tab", "log") == "detail" and reader_target and height < 28:
        detail_body = 0 if reader_target == target else 1
    desired_dock = detail_body + 2 if detail_body else 1
    everyone = {m["participant"]: [c for c in calls if c.get("parent") in
                {m["participant"], m.get("attempt")}] for m in mains}
    dock_args = dict(primary=live_main, main_attempt=main_attempt, everyone=everyone,
                     past=past, attention=ranked)
    dock_lines, dock_hits = tab_box(report, width, desired_dock, navigation, color, unicode,
                                   height=height, **dock_args) if height >= 28 and width >= 20 else ([], [])
    if dock_lines:
        desired_dock = len(dock_lines)
    if width < 20:
        dock_rows = 0
    elif budget - len(head) >= desired_dock + activity_floor:
        dock_rows = desired_dock
    elif height >= 28:
        dock_rows = min(desired_dock, max(1, budget - len(head) - activity_floor))
    else:
        dock_rows = int(budget > len(head))
    if dock_rows == 1 and reading and budget - len(head) - activity_floor >= 3:
        dock_rows = min(desired_dock, budget - len(head) - activity_floor)
    navigation.update(tab_strip_visible=bool(dock_rows), tab_body_visible=dock_rows >= 3)
    activity_head = boxed and budget - len(head) - dock_rows >= 2
    boxed = boxed and budget - len(head) - dock_rows >= 5
    room_budget = max(0, budget - len(head) - dock_rows - (2 if boxed else int(activity_head)))
    body = rows
    unique = list(dict.fromkeys([row["action"] for row in head if row.get("action") and row["action"][0] != "tab"] + items))
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
    scroll_row = len(body) > body_budget and body_budget > body_floor
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
        heading = "Activity%s%s%s main%s%s%s call%s" % (
            sep + "attention" if attention_only else "", sep, len(mains),
            "" if len(mains) == 1 else "s", sep, len(calls), "" if len(calls) == 1 else "s")
        shown.append({"text": box_edge(heading, width, unicode) if boxed else clipped(heading, width),
                      "style": "border"})
    shown += body[offset:offset + body_budget]
    if scroll_row:
        shown.append({"text": clipped("%s-%s / %s rows / %s" % (offset + 1, min(len(body), offset + body_budget), len(body),
                      "j/k to scroll" if menu else "scroll for more"), content_width),
                      "style": "dim", "action": None})
    shown += task_tail
    if boxed:
        shown.append({"text": box_edge("", width, unicode, "bottom"), "style": "border"})
    output, hits = [], []
    previous_action = None
    for y, row in enumerate(shown):
        value = row["text"]
        style = row.get("style")
        inner = boxed and len(head) < y < len(shown) - 1
        if not color and not row.get("spans") and row.get("action") and row["action"] == selected and row["action"] != previous_action:
            span = content_width if inner else width
            if row.get("card_role") and span >= 2 and value[:2] == "  ":
                value = glyph("selected", unicode) + " " + value[2:]
            else:
                value = glyph("selected", unicode) + " " + clipped(value, max(0, span - 2)) if span >= 2 else ">"
        previous_action = row.get("action")
        if inner:
            value = box_edge(value, width, unicode, "divider") if row.get("divider") else box_row(value, width, unicode)
        if color and row.get("spans"):
            styled = "".join(paint(text, token, color=True, colors=color_count(), light=light_theme(),
                                   bold=token in {"main", "judge", "counsel", "worker", "unknown"}) for text, token in row["spans"])
            value = value.replace(row["text"], styled, 1)
        elif color and row.get("tag_span"):
            tag, style_key = row["tag_span"]
            value = value.replace(tag, paint(tag, style_key, color=True, colors=color_count(), light=light_theme()), 1)
        elif color and style:
            value = paint(value, style, color=True, colors=color_count(), light=light_theme(),
                          bold=row.get("action") == selected)
        if row.get("card_role"):
            value = paint_card_edges(value, row["card_role"], unicode=unicode, color=color,
                                     colors=color_count(), light=light_theme())
        output.append(value)
        if row.get("action"):
            hits.append({"y": y + 1, "x1": 1, "x2": width, "action": row["action"], "fold": row.get("fold", False)})
    while output and not output[-1].strip():
        output.pop()
    # The dock takes every row the activity tree leaves.
    spare = max(0, budget - dock_rows - len(output))
    if height >= 28 and dock_rows > 1:
        dock_rows += spare
    else:
        output += [""] * spare
    dock_top = len(output)
    # A hidden dock still runs tab_box so it clears its box, preview and key ownership.
    if len(dock_lines) != dock_rows or not dock_rows:
        dock_lines, dock_hits = tab_box(report, width, dock_rows, navigation, color, unicode, **dock_args)
    if dock_rows == 1:
        dock_lines = [line.replace("╭", "[").replace("╮", "]") for line in dock_lines]
    output += dock_lines
    hits += [dict(hit, y=hit["y"] + dock_top) for hit in dock_hits]
    bands = []
    if navigation.get("box"):
        navigation["box"] = {**navigation["box"], "y1": navigation["box"]["y1"] + dock_top,
                             "y2": navigation["box"]["y2"] + dock_top}
        bands.append(dict(navigation["box"], name="detail", x1=1, x2=width, step=3))
    if navigation.get("log_box"):
        navigation["log_box"] = {"y1": navigation["log_box"]["y1"] + dock_top, "y2": navigation["log_box"]["y2"] + dock_top}
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
            before_plan = next((i for i, action in enumerate(unique) if action == ("tab", "plan")), len(unique))
            unique.insert(before_plan, target)
    navigation.update(hits=hits, items=unique, offset=offset, body_rows=len(body), viewport=body_budget,
                      room_id=room.get("id"), positions=positions, geometry=geometry, surface="tree", bands=bands,
                      tree_selected=selected,
                      primary=("chat", current) if current else None,
                      cancel_owner={"participant": current, "attempt": main_attempt}
                      if current and main_attempt else None)
    # A Past- or Attention-list choice scrolled off its page stays chosen; only the board's own rows are reaped.
    tab = navigation.get("tab")
    kept = (tab in {"past", "needs"} and tab in navigation.get("visible_tabs", [])
            and selected in navigation.get(tab + "_ids", []))
    if selected not in unique and not kept:
        navigation["selected"] = None
        navigation.pop("selected_row", None)
        navigation.pop("row_ids", None)
    return "\n".join(output)


def _detail_line(line, color):
    """Apply semantic colors to already sanitized detail text without changing plain output."""
    if not color:
        return line
    colors, light = color_count(), light_theme()
    if line.startswith(("## ", "OMS / ")) or re.match(r"^[A-Z][A-Za-z ]+:\s*$", line):
        return paint(line, "main", color=True, colors=colors, light=light, bold=True)
    key = re.match(r"^(Task|Outcome|Stage|Next|Changed files|Answer artifact|Result):(?=\s|$)", line)
    if key:
        value = line[key.end():]
        painted_key = paint(key.group(1) + ":", "metadata", color=True, colors=colors, light=light)
        status_roles = {"passed": "good", "pass": "good", "completed": "good", "accepted": "good", "ok": "good",
                        "failed": "bad", "fail": "bad", "rejected": "bad", "blocked": "alert", "error": "bad",
                        "running": "running", "pending": "running", "review": "running"}
        status = re.fullmatch(r"(\s*)(passed|pass|completed|accepted|ok|failed|fail|rejected|blocked|error|running|pending|review)(\s*)",
                              value, flags=re.I)
        if status:
            role = status_roles[status.group(2).lower()]
            return (painted_key + status.group(1) +
                    paint(status.group(2), role, color=True, colors=colors, light=light) + status.group(3))
        line = painted_key + value
    return re.sub(r"(?<![\w])(?:\.?[\w.-]+/)+[\w./-]+", lambda match: paint(
        match.group(0), "dim", color=True, colors=colors, light=light), line)


def render_detail(report, detail, width, height, navigation, managed=False, color=None):
    from panel_view import _result_blocks, _result_structured
    if color is None:
        from panel_render import terminal_style
        color = terminal_style()[1]
    heading = [clipped("OMS / " + detail["title"], width), clipped(usage_line(report, width), width)]
    if "report" in detail:
        from panel_view import render_results
        # The result reader already wrapped and sanitized every line at this width.
        body = render_results(detail["report"], width).split("\n")
    else:
        body = []
        for line, kind in _result_blocks(detail.get("text", "")):
            body.extend(_result_structured(line, width) if kind != "prose" else
                        wrapped(line, width, max(1, len(line)), max_chars=max(400, len(line))))
    activity = detail.get("activity_events")
    activity_call = detail.get("activity_call")
    if activity_call:
        cached = navigation.get("activity_snapshot")
        if not isinstance(cached, dict):
            cached = mapping(mapping(navigation.get("preview")).get("report")).get("activity_snapshot")
        if isinstance(cached, dict):
            activity = mapping(cached.get("calls")).get(activity_call, [])
    what_did = detail.get("what_it_did")
    if activity is not None:
        glyphs = {"say": "›", "think": "~", "run": "$", "edit": "✎", "tool": "·"}
        body.extend(wrapped("Activity:", width, 1))
        if not activity:
            body.extend(wrapped("No activity yet", width, 1))
        for event in activity[-12:]:
            marker = glyphs.get(event.get("kind"), "·")
            if event.get("kind") == "run":
                marker += " ✓" if event.get("ok") is True else " ✗" if event.get("ok") is False else ""
            body.extend(wrapped(marker + " " + event.get("text", ""), width, 1))
    if what_did:
        body.extend(wrapped("What it did:", width, 1))
        for line in what_did:
            body.extend(wrapped(line, width, 1))
    budget = max(0, height - len(heading) - 2)
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - budget))
    navigation.update(hits=[], items=[], offset=offset, viewport=budget, body_rows=len(body), bands=[])
    heading_count = min(len(heading), max(0, height - 2))
    lines = heading[:heading_count] + body[offset:offset + budget]
    lines += [clipped("%s-%s / %s rows" % (offset + 1, min(len(body), offset + budget), len(body)), width),
              clipped("Esc Back  PgUp/PgDn Scroll" + ("  F9 Chat/Board" if managed else "  q Quit"), width)]
    if color and heading_count:
        lines[0] = paint(lines[0], "main", color=True, colors=color_count(), light=light_theme())
        lines[heading_count:-2] = [_detail_line(line, True) for line in lines[heading_count:-2]]
    return "\n".join(lines[-height:])
