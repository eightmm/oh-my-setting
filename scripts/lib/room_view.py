"""Room graph drawn from participation, addressed messages and lifecycle evidence."""

from dashboard_projection import clean, display_width, listing, mapping
from panel_view import (MODEL_NAMES, LIVE_STATES, PALETTE, activity, box_edge, box_row,
                        clipped, location_label, status_alerts, worker_rows, wrapped)


def nodes(report):
    room = mapping(report.get("room"))
    attempts = mapping(report.get("attempts"))
    linked = {}
    rows = listing(attempts.get("active_recent")) + listing(attempts.get("recent"))
    rows = sorted(rows, key=lambda row: mapping(row.get("panel")).get("room_id") != room.get("id"))
    for row in rows:
        meta = mapping(row.get("panel"))
        if meta.get("room_id") and meta["room_id"] != room.get("id"):
            continue
        ident = meta.get("room_participant")
        if ident and not meta.get("room_id") and ident != row.get("attempt_id"):
            continue
        if meta.get("role") == "main" and not ident:
            ident = row.get("attempt_id")
        if ident:
            linked.setdefault(ident, []).append(row)
    output = []
    for member in listing(room.get("participants")):
        if not member.get("joined"):
            continue
        # Dispatched children inherit their main's participant as provenance;
        # only a same-role attempt is the member itself.
        attempt = next((row for row in linked.get(member.get("participant"), [])
                        if mapping(row.get("panel")).get("role") == member.get("role")), {})
        meta = mapping(attempt.get("panel"))
        output.append(dict(member, attempt=attempt.get("attempt_id"), state=attempt.get("state") or "presence unknown",
                           model=meta.get("model") or member.get("model"),
                           task_id=attempt.get("task_id"), location=meta.get("location"),
                           title=meta.get("label") or member.get("label")))
    rank = lambda member: (0 if member["state"] in LIVE_STATES else 2 if member["state"] in {"done", "cancelled"} else 1, -member.get("seq", 0))
    return sorted(output, key=rank)


def card(title, content, width, height, unicode):
    lines = [box_edge(title, width, unicode)]
    for line in content[:height - 2]:
        lines.append(box_row(line, width, unicode))
    lines += [box_row("", width, unicode)] * max(0, height - len(lines) - 1)
    return lines + [box_edge("", width, unicode, "bottom")]


def description(member, frame, unicode, width):
    model = member.get("model")
    name = MODEL_NAMES.get(model, model) or member.get("provider", "unknown")
    title = wrapped(member.get("title") or member["participant"], width - 4, 2)
    return title + ["%s %s / %s" % (activity(member["state"], frame, unicode), name, member["state"]),
                    "@ " + location_label(member.get("location")),
                    "id: " + member["participant"]]


def connections(width, top, bottoms, live, frame, unicode, advisor=None):
    """Only admitted parent IDs draw branches; particles indicate cached activity."""
    if not bottoms and advisor is None:
        return ["", ""]
    bar, upright, tee = ("─", "│", "┬") if unicode else ("-", "|", "+")
    first, second = [" "] * width, [" "] * width
    first[top] = upright
    positions = bottoms + ([advisor] if advisor is not None else [])
    if advisor is not None:
        first[advisor] = upright
    for col in range(min(positions + [top]), max(positions + [top]) + 1):
        second[col] = bar
    second[top] = "┼" if unicode else "+"
    for col in bottoms:
        second[col] = tee
    if advisor is not None:
        second[advisor] = "┴" if unicode else "+"
    if live and frame is not None:
        target = live[(frame // max(1, width // 3)) % len(live)]
        path = list(range(min(top, target), max(top, target) + 1))
        second[path[frame % len(path)]] = "●" if unicode else "o"
    return ["".join(first), "".join(second)]


def render_graph(report, width, height, color=False, unicode=True, frame=None, menu=False,
                 attention_only=False):
    room = mapping(report.get("room"))
    members = nodes(report)
    mains = [m for m in members if m.get("role") == "main"]
    judges = [m for m in members if m.get("role") in ("advisor", "reviewer")]
    workers = [m for m in members if m.get("role") == "worker"]
    if attention_only:
        from panel_view import ATTENTION_STATES
        workers = [m for m in workers if m["state"] in ATTENTION_STATES]
    labels = {m["participant"]: MODEL_NAMES.get(m.get("model"), m.get("model")) or m["label"]
              for m in members}
    active = {m["state"] for m in members}
    phase = ("closed" if room.get("closed") else "verify" if "verifying" in active else
             "work" if active & LIVE_STATES else "review" if "review" in active else
             "decision" if any(m.get("task_id") for m in members) else "prepare")
    lines = []

    def add(value, style=None):
        value = clipped(value, width)
        lines.append(PALETTE[style] + value + "\033[0m" if color and style else value)

    def add_cards(cards, widths, styles):
        for row in zip(*cards):
            cells = []
            for cell, cell_width, style in zip(row, widths, styles):
                cell = clipped(cell, cell_width)
                cell += " " * max(0, cell_width - display_width(cell))
                cells.append(PALETTE[style] + cell + "\033[0m" if color else cell)
            lines.append(" ".join(cells))

    add("OMS / WORK ROOM / " + clean(room.get("title") or room.get("id"), 160), "main")
    for alert in status_alerts(report):
        add(alert, "bad")
    from panel_metrics import header_rows
    for line in header_rows(report, width):
        add(line, "dim")
    add(" > ".join("[" + step.upper() + "]" if step == phase else step
                   for step in ("prepare", "work", "verify", "review", "decision", "closed")), "alert")
    add("Room: %s / %s joined / deliveries at poll or safe point" % (room.get("id"), len(members)), "dim")
    side = max(22, (width - 2) // 4)
    center = width - 2 * side - 2
    widths = [side, center, side]
    owners = {m["participant"] for m in mains} | {m["attempt"] for m in mains if m.get("attempt")}
    debates = [row for row in worker_rows(report) if row["source"] == "council" and row["parent"] in owners]
    lead_lines, judge_lines = [], []
    for row in debates[:1]:
        judge_lines += wrapped(row["work"], side - 4, 2) + [
            "%s %s" % (activity(row["state"], frame, unicode), row["name"]),
            "task: " + row["task"]]
    if len(debates) > 1:
        judge_lines.append("+%s debates / list view" % (len(debates) - 1))
    for index, member in enumerate(mains[:2]):
        if index:
            main_ids = {m["participant"] for m in mains[:2]}
            pair = next((p for p in reversed(listing(room.get("pairs")))
                         if p.get("sender") in main_ids and p.get("recipient") in main_ids), None)
            if pair:
                lead_lines.append("%s > %s / %s pending" % (labels[pair["sender"]], labels[pair["recipient"]], pair["pending"]))
        lead_lines += description(member, frame, unicode, center)[:3]
    for member in judges[:2]:
        judge_lines += description(member, frame, unicode, side)[:3]
    mail = ["Pending: %s deliveries" % room.get("pending_count", 0),
            "Consumed: %s" % room.get("received_count", 0),
            "Answered: %s" % room.get("answered_count", 0),
            "App: " + mapping(room.get("publication")).get("status", "not requested")]
    for pair in listing(room.get("pairs"))[-3:]:
        mail.append("%s > %s: %s" % (labels.get(pair.get("sender"), pair.get("sender")),
                                     labels.get(pair.get("recipient"), pair.get("recipient")), pair.get("pending", 0)))
    add_cards([card("DEBATE / ADVISORS" if debates else "ADVISORS / REVIEW",
                    judge_lines or ["No joined judge"], side, 9, unicode),
               card("MAINS (%s)" % len(mains), lead_lines or ["No main joined"], center, 9, unicode),
               card("MAILBOX", mail, side, 9, unicode)], widths, ["review", "main", "alert"])
    child_widths = [(width - 2) // 3] * 2
    child_widths.append(width - 2 - sum(child_widths))
    centers = [child_widths[0] // 2, child_widths[0] + 1 + child_widths[1] // 2,
               width - child_widths[2] // 2 - 1]
    main_ids = {m["participant"] for m in mains[:2]} | {m["attempt"] for m in mains[:2] if m.get("attempt")}
    attached = [centers[n] for n, m in enumerate(workers[:3]) if m.get("parent") in main_ids]
    advisor_col = side // 2 if debates or any(m.get("parent") in main_ids for m in judges[:2]) else None
    live = [centers[n] for n, m in enumerate(workers[:3])
            if m["state"] in LIVE_STATES and m.get("parent") in main_ids]
    if advisor_col is not None and any(m["state"] in LIVE_STATES and m.get("parent") in main_ids
                                       for m in judges[:2] + debates):
        live.append(advisor_col)
    for line in connections(width, side + 1 + center // 2, attached, live, frame, unicode, advisor_col):
        add(line, "worker")
    cards = []
    for index, child_width in enumerate(child_widths):
        if index < min(len(workers), 3):
            member = workers[index]
            title = "WORKER / " + (labels.get(member["participant"]) or "unknown")
            content = description(member, frame, unicode, child_width)
            if member.get("parent") not in main_ids:
                content[-1] = "Parent outside shown mains"
        else:
            title, content = "WORKER", ["No joined worker"]
        cards.append(card(title, content, child_width, 7, unicode))
    add_cards(cards, child_widths, ["worker"] * 3)
    hidden = max(0, len(mains) - 2) + max(0, len(judges) - 2) + max(0, len(workers) - 3)
    add("+%s participants outside graph / use room show for all" % hidden if hidden else
        "Connections: explicit parents; membership does not prove a process is online", "dim")
    board, scopes = [], []
    tasks = listing(room.get("repo_tasks"))
    total_tasks = mapping(report.get("plan")).get("task_count") or len(tasks)
    for task in tasks[:3 if total_tasks > 4 else 4]:
        board.append("%s / %s%s" % (task.get("id"), task.get("state"),
                                    " / EXPIRED" if task.get("claim_expired") else ""))
    if total_tasks > len(board):
        board.append("+%s tasks / plan details" % (total_tasks - len(board)))
    for member in members:
        if member.get("owns"):
            scopes.append("%s: %s" % (labels[member["participant"]], ", ".join(member["owns"])))
    scopes.append("Isolated patches + parent admission")
    left = (width - 1) // 2
    add_cards([card("REPO TASK BOARD", board or ["No linked plan evidence"], left, 6, unicode),
               card("DECLARED SCOPES / NOT LOCKS", scopes, width - left - 1, 6, unicode)],
              [left, width - left - 1], ["dim", "dim"])
    footer = (["o Rooms  h Chats  g Graph  v Density  r Results  9 Refresh  q Quit"] if menu else [])
    publications = mapping(room.get("publications"))
    if publications:
        footer.append("APP DELIVERY / " + " | ".join("%s: %s%s" %
            (target, receipt.get("status", "unknown"), " (unconfirmed)" if receipt.get("delivery_unknown") else "")
            for target, receipt in list(publications.items())[-3:]))
    footer += ["Motion shows recorded activity; panel refresh does not wake sessions.",
               "Consumption is not approval; call exits are not acceptance."]
    space = max(0, height - len(lines) - len(footer) - int(menu))
    if space:
        add("ROOM LOG / latest addressed messages", "main")
        for message in listing(room.get("messages"))[-max(0, space - 1):] if space > 1 else []:
            if len(lines) >= height - len(footer) - int(menu):
                break
            add("%s > %s / %s: %s" % (labels.get(message.get("sender"), message.get("sender")),
                labels.get(message.get("recipient"), message.get("recipient")), message.get("message_kind"),
                clean(message.get("text"), 240)))
    lines = lines[:max(0, height - len(footer) - int(menu))]
    lines += [clipped(line, width) for line in footer]
    return "\n".join(lines)
