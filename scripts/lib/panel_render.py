"""Terminal frames, status notices and line-oriented presentation."""

import datetime
import os
import re
import shutil
import sys
import textwrap
import time

# Call-local facade imports preserve the public entry point and avoid import cycles.


def show_providers(rows):
    from terminal_panel import clean, clear_control_view
    clear_control_view()
    width = max(1, shutil.get_terminal_size().columns)
    heading = "Registered CLI calls (room delivery: polling / installed hook safe points):"
    print(textwrap.fill(heading, width=width, break_long_words=False, break_on_hyphens=False))
    for row in rows:
        status = "installed" if row["installed"] else "absent"
        native = " / native launch" if row["native_launch"] else ""
        models = (", ".join(row["models"][:4]) or "exact model must be supplied"
                  if row["exact_model_override"] else "native profile controls the model")
        line = "  %s: %s / %s%s / %s" % (row["provider"], status, "/".join(row["access"]), native,
                                         clean(models, 180))
        print(textwrap.fill(line, width=width, subsequent_indent="    ",
                            break_long_words=False, break_on_hyphens=False))


def clear_control_view():
    """Put a line-oriented control view above the board only on an interactive terminal."""
    from terminal_panel import terminal_style
    if terminal_style()[0]:
        print("\033[H\033[2J", end="", flush=True)


def room_listing(rooms, current, width, now=None, live_participants=None):
    """Format bounded discovery snapshots without reading rooms again."""
    from terminal_panel import clean
    from panel_view import box_edge, box_row, clipped
    now = time.time() if now is None else now
    lines = []
    for index, shared in enumerate(rooms, 1):
        participants = shared.get("participants", [])
        main_live = any(bool(p.get("joined")) and p.get("role") == "main" and
                        p.get("participant") in (live_participants or set()) for p in participants)
        unread = sum(len(message.get("pending_for", [])) for message in shared.get("messages", []))
        messages = shared.get("messages", [])
        age = "no messages"
        if messages and messages[-1].get("ts"):
            try:
                stamp = datetime.datetime.fromisoformat(messages[-1]["ts"].replace("Z", "+00:00")).timestamp()
                seconds = max(0, int(now - stamp))
                age = ("%dm ago" % (seconds // 60) if seconds < 3600 else
                       "%dh ago" % (seconds // 3600) if seconds < 86400 else "%dd ago" % (seconds // 86400))
            except (ValueError, TypeError, OverflowError):
                age = "age unknown"
        marker = "*" if shared.get("id") == current else " "
        state = "closed" if shared.get("closed") else "open"
        title = clean(shared.get("title") or "Untitled", 120)
        if shared.get("sealed"):
            line = "%s %s %s -> %s / %s / continued" % (marker, index, clean(shared.get("id", ""), 128),
                                                       clean(shared.get("successor", ""), 128), title)
            lines.append(clipped(line, width))
            continue
        line = "%s %s %s / %s / %s / %d unread / last %s / %s" % (
            marker, index, clean(shared.get("id", ""), 128), title,
            "main live" if main_live else "no live main", unread, age, state)
        lines.append(clipped(line, width))
    if width >= 20:
        return [box_edge("OMS · Rooms", width)] + [box_row(line, width) for line in lines] + [
            box_edge("", width, kind="bottom")]
    return lines


def terminal_style():
    capable = sys.stdout.isatty() and os.environ.get("TERM") not in (None, "dumb")
    encoding = sys.stdout.encoding or "ascii"
    try:
        "✳╭╰├─│".encode(encoding)
        unicode = os.environ.get("TERM") != "dumb"
    except (LookupError, UnicodeEncodeError):
        unicode = False
    preference = os.environ.get("OMS_PANEL_COLOR", "auto")
    color = preference == "always" or (preference != "never" and "NO_COLOR" not in os.environ)
    return capable, capable and color, unicode


def managed_session():
    from terminal_panel import PANEL_SESSION
    return bool(os.environ.get("TMUX") and re.fullmatch(
        PANEL_SESSION, os.environ.get("OMS_PANEL_SESSION", "")))


def stale_notice(navigation, notice):
    """Show or withdraw the stale-data notice without replacing another notice."""
    navigation["capacity_stale"] = bool(notice)
    mine = navigation.pop("stale_notice", None)
    if mine is not None and navigation.get("notice") == mine:
        navigation.pop("notice", None)
    if notice and "notice" not in navigation:
        navigation["notice"] = navigation["stale_notice"] = notice


AVAILABLE = {}


def provider_availability(state):
    """PATH scans once per snapshot, not once per animation frame."""
    from terminal_panel import AVAILABLE, PROVIDERS
    if AVAILABLE.get("state") is not state:
        AVAILABLE.update(state=state, found={name: bool(shutil.which(name)) for name in PROVIDERS})
    return AVAILABLE["found"]


def frame_view(repo, provider, state, previous=None, menu=True, main_attempt=None, frame=None,
               view="auto", attention_only=False, navigation=None):
    from terminal_panel import managed_session, provider_availability, refresh_failed, render, terminal_style
    try:
        size = os.get_terminal_size(sys.stdout.fileno())
    except (OSError, ValueError):
        size = shutil.get_terminal_size((100, 28))
    capable, color, unicode = terminal_style()
    if navigation is not None and navigation.get("capacity_stale"):
        state = dict(state, capacity_stale=True, capacity_alerts=[])
    if state.get("collection", {}).get("pending"):
        from panel_view import clipped
        text = "\n".join(clipped(line, size.columns) for line in
                         ("OMS / Loading work room...", "Keys remain available while status loads")[:max(0, size.lines)])
    elif navigation is not None and navigation.get("health_view"):
        import panel_health
        text = panel_health.render(state, size.columns, size.lines)
    elif navigation is not None and navigation.get("detail") and not refresh_failed(state):
        from panel_tree import render_detail
        text = render_detail(state, navigation["detail"], size.columns, size.lines, navigation, managed_session())
    else:
        text = render(state, provider, size.columns, size.lines, color=color, unicode=unicode,
                      previous=previous, availability=provider_availability(state),
                      menu=menu, managed=managed_session(), main_attempt=main_attempt, frame=frame,
                      view=view, attention_only=attention_only, navigation=navigation)
    return text, size, capable


def _show_state(repo, main_attempt=None, navigation=None, cache=None):
    # A cache lets key presses redraw the last snapshot instead of collecting a new one.
    from terminal_panel import hold_state, snapshot, stale_notice
    if cache is not None and "state" in cache:
        state, status = cache["state"], cache["status"]
    else:
        state, status = snapshot(repo)
        if cache is not None:
            state, status, notice = hold_state(cache.setdefault("held", {}), state, status, os.environ.get("OMS_ROOM_ID", ""))
            if navigation is not None:
                stale_notice(navigation, notice)
        from panel_metrics import collect
        if state.get("room", {}).get("participants"):
            state = dict(state, provider_status=collect(state, main_attempt, repo=repo))
        if cache is not None:
            cache.update(state=state, status=status)
    return state, status


def show(repo, provider, previous=None, menu=True, clear=True, main_attempt=None,
         view="auto", attention_only=False, navigation=None, cache=None):
    from terminal_panel import _show_state, frame_view
    state, status = _show_state(repo, main_attempt, navigation, cache)
    text, size, capable = frame_view(repo, provider, state, previous, menu, main_attempt,
                                    view=view, attention_only=attention_only, navigation=navigation)
    # Finish collection/rendering before clearing, so errors never blank the pane.
    if capable and clear:
        # Repaint in place: lines are overwritten and the rest erased, so a refresh never blanks the pane.
        print("\033[H" + text.replace("\n", "\033[K\n") + "\033[K\033[J", flush=True)
    else:
        print(text, flush=True)
    return status


def draw_frame(view, size, capable, last=None):
    if not capable:
        print(view, flush=True)
    elif last is None or last[1] != size or len(last[0].splitlines()) != len(view.splitlines()):
        sys.stdout.write("\033[H\033[2J" + view + "\033[J")
        sys.stdout.flush()
    else:
        for index, (before, after) in enumerate(zip(last[0].splitlines(), view.splitlines())):
            if before != after:
                sys.stdout.write("\033[%s;1H\033[2K%s" % (index + 1, after))
        sys.stdout.flush()
    return view, size
