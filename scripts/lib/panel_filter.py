"""Board call filter: `/` narrows the visible call rows, Esc clears it.

Display only. The filter hides rows from the board tree; it never changes the
snapshot, the Attention queue or its header counts, so a call that waits on the
owner stays counted whatever the filter shows.
"""

MAX_QUERY = 40
FIELDS = ("title", "role", "purpose", "provider", "model", "alias", "state")


def query(navigation):
    value = navigation.get("filter")
    return value if isinstance(value, str) else ""


def active(navigation):
    return bool(query(navigation).strip())


def editing(navigation):
    return navigation.get("filter_editing") is True


def start(navigation):
    """Begin typing; False when the board list is not what the screen shows."""
    if navigation.get("detail") or navigation.get("health_view") or navigation.get("cancel_pending"):
        return False
    navigation["filter"] = query(navigation)
    navigation["filter_editing"] = True
    return True


def clear(navigation):
    navigation.pop("filter", None)
    navigation.pop("filter_editing", None)
    navigation["offset"] = 0
    navigation.pop("band_offsets", None)


def edit(event, navigation):
    """Typing keys; True when the event was consumed. Arrows, Tab and clicks pass through."""
    kind = event[0]
    if kind == "escape":
        clear(navigation)
        return True
    if kind == "enter":
        navigation.pop("filter_editing", None)
        if not active(navigation):
            clear(navigation)
        return True
    text = query(navigation)
    if kind == "backspace":
        text = text[:-1]
    elif len(kind) == 1 and kind.isprintable():
        text = (text + kind)[:MAX_QUERY]
    else:
        return False
    navigation["filter"] = text
    navigation["offset"] = 0
    navigation.pop("band_offsets", None)
    return True


def matches(call, text):
    """Every word of the query occurs in the call's title, role, provider, model or state."""
    words = text.casefold().split()
    haystack = " ".join(str(call.get(key) or "") for key in FIELDS).casefold().replace("_", " ")
    return all(word in haystack for word in words)


def apply(calls, navigation, classify, urgent_states):
    """`(kept, stats)`; stats is None without a query, else (shown, total, hidden calls that wait on the owner)."""
    text = query(navigation)
    if not text.strip():
        return calls, None
    kept = [call for call in calls if matches(call, text)]
    ids = {id(call) for call in kept}
    open_calls = [call for call in calls if classify(call) != "past"]
    hidden = sum(id(call) not in ids and (call["state"] in urgent_states or classify(call) == "review")
                 for call in open_calls)
    return kept, (sum(classify(call) != "past" for call in kept), len(open_calls), hidden)


def heading(navigation, stats, width, unicode):
    """`Filter /fail_ · shown 2 of 9 · 1 waiting hidden`, or None; the second value is True when a waiting call is hidden."""
    if not active(navigation) and not editing(navigation):
        return None
    from panel_view import clipped, display_width
    sep = " · " if unicode else " / "
    cursor = ("▏" if unicode else "_") if editing(navigation) else ""
    lead = "Filter /" + query(navigation) + cursor
    hidden = stats[2] if stats else 0
    tail = "Enter keeps, Esc clears" if editing(navigation) else "Esc clears"
    if stats:
        shown = "shown %d of %d" % stats[:2]
        waiting = "%d waiting hidden" % hidden if hidden else ""
        forms = [[shown, waiting, tail], [shown, waiting], ["%d/%d" % stats[:2], "%d hidden!" % hidden if hidden else ""],
                 ["%d hidden!" % hidden if hidden else ""], []]
    else:
        forms = [["type to narrow calls", tail], ["type to narrow calls"], []]
    for form in forms:
        text = sep.join([lead] + [part for part in form if part])
        if display_width(text) <= width:
            break
    return clipped(text, width), bool(hidden)
