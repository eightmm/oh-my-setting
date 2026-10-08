"""Between tab for the panel board: the mains on a regular polygon, mail counts written on every connection.

Pure drawing of the pair counts the board already holds (room_view.main_links); reading writes nothing.
Rows are (text, action, selected, extra); extra carries the per-cell hits and colour spans of the canvas.
"""

import math

from dashboard_projection import clean, display_width, listing, mapping

MAX_POLYGON = 6
MIN_ROWS = {2: 1, 3: 7, 4: 9, 5: 11, 6: 13}
# First vertex angle (degrees, clockwise from the right): triangle and pentagon point up, the square sits flat,
# the hexagon has a flat top so its widest points share a row.
START = {3: -90, 4: -135, 5: -90, 6: -120}


def _cell(text):
    return "".join(ch if display_width(ch) == 1 else "?" for ch in clean(text, 30))


def _count(link, flip, unicode):
    forward, back = (link["back"], link["forward"]) if flip else (link["forward"], link["back"])
    text = ("%d→ ←%d" if unicode else "%d> <%d") % (forward, back)
    return text + ((" · %d new" if unicode else " / %d new") % link["new"] if link["new"] else "")


def _line(a, b):
    """Cells from a to b, Bresenham."""
    (x0, y0), (x1, y1) = a, b
    dx, dy, sx, sy = abs(x1 - x0), -abs(y1 - y0), 1 if x0 < x1 else -1, 1 if y0 < y1 else -1
    error, cells = dx + dy, []
    while True:
        cells.append((x0, y0))
        if (x0, y0) == (x1, y1):
            return cells
        doubled = 2 * error
        if doubled >= dy:
            error, x0 = error + dy, x0 + sx
        if doubled <= dx:
            error, y0 = error + dx, y0 + sy


def _stroke(a, b, unicode, dotted):
    if dotted:
        return "·" if unicode else "."
    dx, dy = abs(b[0] - a[0]), abs(b[1] - a[1]) * 2
    if dy * 5 < dx * 2:
        return "─" if unicode else "-"
    if dx * 5 < dy * 2:
        return "│" if unicode else "|"
    if (b[0] - a[0]) * (b[1] - a[1]) > 0:
        return "╲" if unicode else "\\"
    return "╱" if unicode else "/"


def _short(label):
    first = label.split(" ", 1)[0]
    return first if first.startswith("#") else label


def _compact(links, labels, cap, unicode):
    joiner = " ⇄ " if unicode else " <> "
    shown = links if len(links) <= cap else links[:max(0, cap - 1)]
    rows = [("%s%s%s  %s" % (_short(labels[l["left"]]), joiner, _short(labels[l["right"]]), _count(l, False, unicode)),
             ("pair", (l["left"], l["right"])), False) for l in shown]
    if len(shown) < len(links):
        rows.append(("+%d more pair%s" % (len(links) - len(shown), "" if len(links) - len(shown) == 1 else "s"), None, False))
    return rows or [("No mail between mains yet", None, False)]


def _polygon(mains, links, labels, own, width, height, unicode):
    """(canvas, styles, cells), or None when a label cannot be placed intact."""
    count = len(mains)
    texts = [_cell(("[%s]" if m == own else "%s") % labels[m]) for m in mains]
    reach = max(map(len, texts))
    if width < reach + 4:
        return None
    angles = [180.0 + 180.0 * k for k in range(2)] if count == 2 else [
        START[count] + 360.0 * k / count for k in range(count)]
    unit = [(math.cos(math.radians(a)), math.sin(math.radians(a))) for a in angles]
    span_x, span_y = max(abs(u[0]) for u in unit), max(abs(u[1]) for u in unit)
    room_x = (width - 1 - reach) / 2.0 / span_x
    if span_y > 1e-9:
        # Terminal cells are about twice as tall as wide, so the horizontal radius is twice the vertical one.
        scale_y = min((height - 1) / 2.0 / span_y, room_x / 2.0)
        scale_x = 2 * scale_y
    else:
        scale_x, scale_y = min(room_x, 30.0), 0.0
    cx, cy = (width - 1) / 2.0, (height - 1) / 2.0
    centers = [(int(round(cx + scale_x * u[0])), int(round(cy + scale_y * u[1]))) for u in unit]
    canvas = [[" "] * width for unused in range(height)]
    style = [[None] * width for unused in range(height)]
    taken, cells = set(), []

    def put(col, row, text, how):
        for i, ch in enumerate(text):
            canvas[row][col + i] = ch
            style[row][col + i] = how
            taken.add((col + i, row))

    boxes = []
    for (x, y), text in zip(centers, texts):
        start = min(max(0, x - (len(text) - 1) // 2), width - len(text))
        boxes.append((start, y))
        if any((start + i, y) in taken for i in range(len(text))):
            return None
        taken.update((start + i, y) for i in range(len(text)))
    order = {m: i for i, m in enumerate(mains)}
    for l in links:
        a, b = centers[order[l["left"]]], centers[order[l["right"]]]
        dotted = not (l["forward"] or l["back"] or l["new"])
        if dotted and count > 4:
            continue
        mark = _stroke(a, b, unicode, dotted)
        for x, y in _line(a, b):
            if (x, y) not in taken and 0 <= x < width and 0 <= y < height:
                canvas[y][x], style[y][x] = mark, "dim" if dotted else None
    for l in links:
        if not (l["forward"] or l["back"] or l["new"]):
            continue
        a, b = centers[order[l["left"]]], centers[order[l["right"]]]
        flip = (a[0], a[1]) > (b[0], b[1])
        text = _count(l, flip, unicode)
        if len(text) > width:
            return None
        path = _line(a, b)
        placed = None
        for t in (0.5, 0.4, 0.6, 0.3, 0.7, 0.2, 0.8, 0.1, 0.9):
            x, y = path[int(round(t * (len(path) - 1)))]
            for lift in (0, -1, 1, -2, 2):
                col, row = min(max(0, x - len(text) // 2), width - len(text)), y + lift
                if 0 <= row < height and not any((col + i, row) in taken for i in range(len(text))):
                    placed = (col, row)
                    break
            if placed:
                break
        if not placed:
            return None
        put(placed[0], placed[1], text, "alert" if l["new"] else None)
        cells.append((placed[1], placed[0], placed[0] + len(text) - 1, ("pair", (l["left"], l["right"]))))
    for m, (start, y), text in zip(mains, boxes, texts):
        put(start, y, text, "own" if m == own else "vertex")
        cells.append((y, start, start + len(text) - 1, ("chat", m)))
    return canvas, style, cells


def tab_body(report, width, capacity, navigation, unicode=True):
    """Rows for the Between tab; the polygon when the box is big enough, else one line per pair."""
    from room_view import main_links, main_names, nodes, window_order
    room = mapping(report.get("room"))
    members = nodes(report)
    mains = window_order(report, [m for m in members if m.get("role") == "main"])
    if len(mains) < 2:
        return [("Only one main — nothing between mains yet" if unicode else "Only one main - nothing between mains yet",
                 None, False)][:capacity]
    labels = main_names(report, members)
    ids = [m["participant"] for m in mains]
    links = main_links(room, mains)
    owner = next((m["participant"] for m in mains if navigation.get("main_attempt") in (m["participant"], m.get("attempt"))), None)
    drawn = None
    if len(ids) <= MAX_POLYGON and capacity >= MIN_ROWS[len(ids)]:
        drawn = _polygon(ids, links, labels, owner, width, capacity, unicode)
    if not drawn:
        return _compact(links, labels, capacity, unicode)
    canvas, style, cells = drawn
    provider = {m["participant"]: "codex" if m.get("provider") == "codex" else "main" for m in mains}
    names = {}
    for row, x1, x2, action in cells:
        if action[0] == "chat":
            names[(row, x1)] = provider[action[1]]
    rows = []
    for y, line in enumerate(canvas):
        spans, x = [], 0
        while x < width:
            how = style[y][x]
            if how is None:
                x += 1
                continue
            end = x
            while end + 1 < width and style[y][end + 1] == how:
                end += 1
            if how in ("vertex", "own"):
                spans.append((x, end + 1, names.get((y, x), "main"), how == "own"))
            else:
                spans.append((x, end + 1, how, False))
            x = end + 1
        rows.append(("".join(line).rstrip(), None, False,
                     {"cells": [(x1, x2, a) for r, x1, x2, a in cells if r == y], "spans": spans}))
    return rows
