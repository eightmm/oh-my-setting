"""Connected room tree and a frame-local map of navigation targets."""

from dashboard_projection import clean, listing, mapping
from panel_metrics import header_rows
from panel_view import (ATTENTION_STATES, MODEL_NAMES, PALETTE, activity, clipped,
                        status_alerts, tone, usage_labels, worker_rows, wrapped)
from room_view import nodes


def render_tree(report, width, height, color=False, unicode=True, frame=None, menu=False,
                attention_only=False, main_attempt=None, navigation=None):
    navigation = navigation if navigation is not None else {}
    selected = navigation.get("selected")
    collapsed = navigation.get("collapsed", set())
    room = mapping(report.get("room"))
    members = nodes(report)
    current = next((m["participant"] for m in members if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)), None)
    mains = sorted([m for m in members if m.get("role") == "main"],
                   key=lambda m: (m["participant"] != current, m.get("seq", 0), m["participant"]))
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

    def add(text, style=None, action=None, fold=False):
        rows.append({"text": clipped(text, width), "style": style, "action": action, "fold": fold})
        if action:
            items.append(action)

    add("OMS / WORK ROOM / " + (clean(room.get("title") or room.get("id")) or "select a room"), "main")
    for index, text in enumerate(header_rows(report, width)):
        add(text, "review" if index == 0 else "main")
    for alert in status_alerts(report):
        add(alert, "bad")
    add("MAILBOX / pending %s / consumed %s / answered %s" % (
        room.get("pending_count", 0), room.get("received_count", 0), room.get("answered_count", 0)),
        "alert" if room.get("pending_count") else "dim")
    add("OMS activity / " + ("attention" if attention_only else "room") + " / tree", "dim")
    header = len(rows)
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
        add("%s %s%s %s / %s%s%s" % (toggle, end if last_main else prefix,
            activity(main["state"], frame, unicode), model,
            main["state"], " / current" if main["participant"] == current else "",
            " / %s calls" % len(attached) if folded else ""), "main", action, bool(attached))
        title = main.get("title")
        if title and title != "Native task not recorded":
            for line in wrapped(title, max(1, width - 7), 2):
                add(root_stem + "  " + line, "dim", action)
        if not folded:
            groups = [(role, [m for m in attached if m.get("role") == role])
                      for role in ("council", "advisor", "reviewer", "worker")]
            groups = [(role, children) for role, children in groups if children]
            for group_index, (role, children) in enumerate(groups):
                last_group = group_index == len(groups) - 1
                heading = "COUNCIL" if role == "council" else role.upper() + "S"
                add(root_stem + (end if last_group else prefix) + heading + " (%s)" % len(children), "dim")
                indent = root_stem + ("   " if last_group else stem)
                for child_index, child in enumerate(children):
                    branch = end if child_index == len(children) - 1 else prefix
                    model = MODEL_NAMES.get(child.get("model"), child.get("model")) or child.get("provider", "unknown")
                    label = child.get("title") or child["participant"]
                    add(indent + branch + "%s %s / %s / %s" % (
                        activity(child["state"], frame, unicode), model, label, child["state"]),
                        tone(child["state"]), child.get("_action") or ("result", child["participant"]))
        if n < len(mains) - 1:
            add("  │" if unicode else "  |", "dim")
    unlinked = [m for m in calls if m.get("parent") not in roots]
    if unlinked:
        add("UNLINKED / parent outside shown room mains", "alert")
        for n, child in enumerate(unlinked):
            add((end if n == len(unlinked) - 1 else prefix) + "%s %s / %s" % (
                activity(child["state"], frame, unicode), child.get("title") or child["participant"], child["state"]),
                tone(child["state"]), ("result", child["participant"]))
    if not mains and not calls:
        add("No joined participants" if room.get("id") else "Select a work room to navigate its participants", "dim")
    tasks = listing(room.get("repo_tasks"))
    if tasks:
        add("REPO TASK BOARD / repository scope", "dim")
        for n, task in enumerate(tasks):
            add((end if n == len(tasks) - 1 else prefix) + "%s / %s / %s" % (
                task.get("id"), task.get("state"), task.get("title") or "title unrecorded"),
                "dim", ("task", task.get("id")))
    acceptance = mapping(report.get("acceptance"))
    if acceptance.get("available"):
        add("REPO ACCEPT / %s/%s verified" % (mapping(acceptance.get("counts")).get("verified", 0),
            acceptance.get("total", "?")), "good" if acceptance.get("complete") else "alert")
    for message in listing(room.get("messages"))[-2:]:
        add("ROOM LOG / " + (clean(message.get("text")) or ""), "dim")
    usage = usage_labels(report)
    if usage and height >= 30:
        add("USAGE recent8 / " + " | ".join(usage), "dim")
    add("DECLARED SCOPES / labels, not locks", "dim")
    for main in mains:
        if main.get("owns"):
            add("  " + (main.get("provider") or "main") + ": " + ", ".join(main["owns"]), "dim")
    footer = (["MAIN  1 Codex  2 Claude  3 Resume  4 Main  t Task",
               "TASK  5 Explain  6 Implement  p CLIs",
               "JUDGE a Advisor  7 Review  c Council",
               "RESULT r Read  h Chats  f Finalize  n Retry delivery",
               "VIEW  o Rooms  g Graph  v Density  b Attention  9 Refresh  q Quit"] if menu else [])
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    if not menu:
        footer.append("Click: chat/result  arrows + Enter  Space: fold  wheel: scroll")
    footer.append("OMS records; call exits are not acceptance")
    footer_space = max(0, height - int(menu) - 1)
    footer = footer[-footer_space:] if footer_space else []
    budget = max(0, height - int(menu) - len(footer))
    head = rows[:min(header, budget)]
    room_budget = max(0, budget - len(head))
    body = rows[header:]
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - room_budget))
    if len(body) > room_budget and room_budget:
        room_budget -= 1
        offset = min(offset, max(0, len(body) - room_budget))
    shown = head + body[offset:offset + room_budget]
    if len(body) > room_budget and budget > len(head):
        shown.append({"text": clipped("%s-%s / %s rows / scroll for more" % (offset + 1, min(len(body), offset + room_budget), len(body)), width),
                      "style": "dim", "action": None})
    output, hits = [], []
    unique = list(dict.fromkeys(items))
    positions = {}
    for index, row in enumerate(body):
        if row.get("action"):
            positions.setdefault(row["action"], index)
    for y, row in enumerate(shown):
        value = row["text"]
        style = row.get("style")
        if color and row.get("action") and row["action"] == selected:
            value = "\033[7m" + value + "\033[0m"
        elif color and style:
            value = PALETTE[style] + value + "\033[0m"
        elif row.get("action") and row["action"] == selected:
            value = clipped(value, max(0, width - 2)) + " <" if width >= 2 else "<"
        output.append(value)
        if row.get("action"):
            hits.append({"y": y + 1, "x1": 1, "x2": width, "action": row["action"], "fold": row.get("fold", False)})
    output += [clipped(line, width) for line in footer]
    navigation.update(hits=hits, items=unique, offset=offset, body_rows=len(body), viewport=room_budget,
                      room_id=room.get("id"), positions=positions)
    return "\n".join(output)


def render_detail(report, detail, width, height, navigation):
    heading = ["OMS / " + detail["title"]] + header_rows(report, width)
    body = [clipped(line, width) for line in detail["text"].splitlines()]
    budget = max(0, height - len(heading) - 2)
    offset = min(max(0, navigation.get("offset", 0)), max(0, len(body) - budget))
    navigation.update(hits=[], items=[], offset=offset, viewport=budget)
    lines = heading[:max(0, height - 2)] + body[offset:offset + budget]
    lines += ["%s-%s / %s rows" % (offset + 1, min(len(body), offset + budget), len(body)),
              "Esc: tree  wheel / PgUp / PgDn: scroll  q: quit watcher"]
    return "\n".join(clipped(line, width) for line in lines[-height:])
