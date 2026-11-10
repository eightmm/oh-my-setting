"""Key tables the control menu, its help and the result reader render their hints from.

The board's own table is panel_layout.PANEL_KEYS; this module holds the other two
surfaces so a hint and the key it names are declared once. Leaf module: it imports
nothing at load time so panel_view and panel_reader can both use it.
"""

# (group, key, label). The START harness rows and the swap row are filtered by the caller.
CONTROL_KEYS = (
    ("START", "1", "Claude"), ("START", "2", "Pi"), ("START", "0", "Codex"),
    ("START", "3", "Resume"), ("START", "t", "Task"),
    ("OTHER", "s", "Swap main"), ("OTHER", "o", "Rooms"), ("OTHER", "k", "Capacity"),
    ("OTHER", "9", "Refresh"), ("OTHER", "?", "More"), ("OTHER", "q", "Quit"),
    ("OPEN", "h", "Chats"), ("OPEN", "r", "Results"), ("OPEN", "g", "Graph"), ("OPEN", "z", "Panel"),
    ("TERMINAL", "j", "Terminals and jobs"),
    ("WORK", "5", "Explain"), ("WORK", "6", "Implement"), ("WORK", "e", "Researcher"),
    ("WORK", "a", "Advisor"), ("WORK", "7", "Review"), ("WORK", "c", "Council"),
    ("SETUP", "4", "Main provider"), ("SETUP", "p", "CLIs"), ("SETUP", "8", "Dashboard"),
    ("RESULT", "f", "Finalize"), ("RESULT", "n", "Retry delivery"),
)
START_HARNESS = {"1": "claude", "2": "pi", "0": "codex"}


def control_text(group, quit_label="Quit", occupied=True, installed=None, short_more=False, only=None):
    """One group's `[k] Label` hints, two spaces apart; the same text feeds every menu size."""
    parts = []
    for name, key, label in CONTROL_KEYS:
        if name != group or (only is not None and key not in only):
            continue
        if key == "s" and not occupied:
            continue
        if key in START_HARNESS and installed is not None and START_HARNESS[key] not in installed:
            continue
        label = quit_label if key == "q" else label
        parts.append("[%s]" % key if short_more and key == "?" else "[%s] %s" % (key, label))
    return "  ".join(parts)


def control_keys():
    """Every control-menu key once, in table order."""
    return list(dict.fromkeys(key for unused, key, unused_label in CONTROL_KEYS))


# (key, label, rank): lower rank survives a narrow pane longer. Order here is display order.
READER_KEYS = (
    ("Tab", "View", 1), ("r", "Raw", 6), ("e", "Events", 7), ("d", "Diff", 8), ("i", "Info", 9),
    ("x", "Transcript", 2), ("[ ]", "Section", 4), ("o", "Outline", 5), ("/", "Search", 3),
    ("n/N", "Match", 10), ("z", "Zoom", 11), ("Space", "Page", 12), ("q/Esc", "close", 0),
)


def fit_hints(hints, width, separator="  "):
    """`(rank, text)` pairs in display order: drop whole hints, lowest priority first, until the row fits.

    A hint is never cut mid-word; when nothing fits the top-priority hint is returned alone.
    """
    from panel_view import clipped, display_width
    kept = list(hints)
    while len(kept) > 1 and display_width(separator.join(text for unused, text in kept)) > width:
        kept.remove(max(kept, key=lambda item: item[0]))
    return clipped(separator.join(text for unused, text in kept), width)


def reader_hint(width, zoomable=False, run_info=False):
    """The result reader's key row for a pane `width` columns wide."""
    hints = []
    for key, label, rank in READER_KEYS:
        if key == "z" and not zoomable:
            continue
        if key == "i" and run_info:
            label = "Run info"
        hints.append((rank, key + " " + label))
    return fit_hints(hints, width)
