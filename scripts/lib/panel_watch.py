"""Background reads, window indicators and the terminal watcher."""

import json
import os
import panel_debate
import panel_host
import panel_messages
import panel_reader
import queue
import re
import signal
import subprocess
import sys
import threading
import time

# Call-local facade imports preserve the public entry point and avoid import cycles.


class BackgroundRead:
    """At most one bounded read; terminal input and drawing stay on the caller."""
    def __init__(self):
        self.ready = queue.Queue(maxsize=1)
        self.pending = False

    def start(self, reader):
        if self.pending:
            return
        self.pending = True

        def run():
            try:
                result = reader()
            except Exception as error:
                result = error
            self.ready.put(result)

        threading.Thread(target=run, daemon=True).start()

    def take(self):
        try:
            result = self.ready.get_nowait()
        except queue.Empty:
            return False, None
        self.pending = False
        return True, result


def mark_window(attention, asked=False):
    """Prefix this watcher's own OMS window with "! " while its main needs attention, else "? " while a question
    to it is open; no-op when unchanged/unsafe."""
    from terminal_panel import PROVIDERS, managed_session, tmux_command
    pane = os.environ.get("TMUX_PANE", "")
    if not managed_session() or not re.fullmatch(r"%[0-9]+", pane):
        return
    mark = "! " if attention else "? " if asked else ""
    try:
        result = subprocess.run(tmux_command("display-message", "-p", "-t", pane, "#{window_name}"),
                                capture_output=True, text=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
        name = result.stdout.rstrip("\n") if result.returncode == 0 else ""
        base = name[2:] if name[:2] in ("! ", "? ") else name
        if base not in PROVIDERS or name == mark + base:
            return
        subprocess.run(tmux_command("rename-window", "-t", pane, mark + base),
                       capture_output=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        pass


def start_spawn(repo, provider, spawns, navigation, swap=False):
    """Start a main off the input loop; a second request waits for the first."""
    from terminal_panel import ENTRY, clean
    if any(entry[0].poll() is None for entry in spawns):
        navigation["notice"] = "A main start or swap is already in progress"
        return
    env = os.environ.copy()
    env.pop("OMS_ROOM_PARTICIPANT", None)
    try:
        proc = subprocess.Popen(["bash", str(ENTRY), "panel", "--repo", str(repo),
                                 "--swap-main" if swap else "--spawn-main", provider, "--json"],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, env=env)
    except OSError as error:
        navigation["notice"] = clean(str(error), 160)
        return
    spawns.append((proc, provider, time.monotonic(), swap))
    navigation["notice"] = ("Waiting for handoff to %s..." if swap else "Starting %s main...") % provider.capitalize()


def reap_spawns(spawns, navigation):
    """Report finished spawns; True when one finished, so the board re-reads the room."""
    from terminal_panel import clean
    done = False
    for entry in list(spawns):
        proc, provider, started, swap = entry
        if not swap and proc.poll() is None and time.monotonic() - started > 60:
            proc.kill()
        if proc.poll() is None:
            continue
        spawns.remove(entry)
        out, err = proc.communicate()
        try:
            report = json.loads(out)
            navigation["notice"] = ("Swapped to %s in window %s" if swap else "Started %s main in window %s") % (
                provider.capitalize(), report["window"])
        except (ValueError, KeyError, TypeError):
            navigation["notice"] = clean((err.strip().splitlines() or ["main did not start"])[-1], 160)
        done = True
    return done


def watch(repo, provider, count=0, no_animation=False, view=None, attention_only=False):
    from terminal_panel import BOTTOM_OPTION, BackgroundRead, MAIN_HARNESSES, PanelModeSync, REJECTED_RELOAD
    from terminal_panel import ReloadWatcher, SplitSync, _clear_cancel_option, _poll_cancel_confirmation
    from terminal_panel import _request_cancel_confirmation, ask_advisor, board_view, bottom_state, clean
    from terminal_panel import clear_control_view, cycle_panel_mode, expire_notice, finalize_selection
    from terminal_panel import finish_opening, hold_state, main_has_open_questions, main_installed
    from terminal_panel import main_needs_attention, managed_session, mark_window, navigate, open_worktree
    from terminal_panel import panel_geometry, publish_bottom, read_evidence, read_panel, reap_spawns
    from terminal_panel import refreshed_navigation, reload_ready, reset_group_view, session_option
    from terminal_panel import source_stamps, stale_notice, start_spawn, swap_choices, tab_event
    from terminal_panel import terminal_style, tmux_command, toggle_context_headers, update_context_header
    from panel_navigation import accept_activity_report
    from panel_input import TerminalInput, choose
    n, frame, last = 0, 0, None
    capable = terminal_style()[0]
    motion = capable and not no_animation and os.environ.get("OMS_PANEL_NO_ANIMATION") != "1"
    navigation = {"collapsed": set(), "offset": 0}
    if os.environ.get("TMUX"):
        # A --launch panel has no control window; F5 then has nowhere to go, so the board does not offer it.
        try:
            names = subprocess.run(tmux_command("list-windows", "-F", "#{window_name}"), capture_output=True,
                                   text=True, check=False, timeout=3, stdin=subprocess.DEVNULL).stdout.split()
            navigation["control_window"] = "control" in names
        except (OSError, subprocess.SubprocessError):
            pass
    try:
        kept = json.loads(os.environ.pop("OMS_PANEL_RESUME", "") or "{}")
    except ValueError:
        kept = {}
    def as_tuple(value):
        return tuple(as_tuple(item) for item in value) if isinstance(value, list) else value
    if isinstance(kept, dict) and kept.get("room_id"):
        navigation.update({key: as_tuple(value) for key, value in kept.items()
                           if key in ("room_id", "selected", "dismissed", "tab") and value is not None})
        if kept.get("preview"):
            navigation["preview"] = {"target": as_tuple(kept["preview"])}
    saved_handlers = {}
    expanded = False
    stamps = source_stamps()
    flagged = None
    spawns = []
    installed = [name for name in MAIN_HARNESSES if main_installed(name)]

    def expand(enabled):
        from terminal_panel import managed_session, native_repository, tmux_command, window_zoomed
        nonlocal expanded
        if expanded == enabled:
            return
        if panel_host.herdr_active():
            panel_host.current(native_repository(repo))
            panel_host.zoom(enabled)
        elif managed_session():
            # resize-pane -Z toggles; the person may have unzoomed with prefix+z or by selecting another pane.
            if window_zoomed() is not enabled:
                subprocess.run(tmux_command("resize-pane", "-Z", "-t", os.environ.get("TMUX_PANE", "")),
                               check=True, timeout=3, stdin=subprocess.DEVNULL)
        expanded = enabled

    def open_call(target):
        """Show a recorded call in the reader pane right of the board; False keeps its detail in the board."""
        from terminal_panel import managed_session, reader_shell
        if not managed_session() or panel_host.herdr_active():
            navigation["notice"] = "Right reader unavailable at this size/host"
            return False
        board = os.environ.get("TMUX_PANE", "")
        try:
            pane = panel_reader.open_reader(board, reader_shell(repo, target, board, navigation.get("room_id")), repo)
            if pane and expanded:
                expand(False)  # splitting a zoomed window unzooms it
        except (OSError, ValueError, subprocess.SubprocessError):
            navigation["notice"] = "Right reader unavailable at this size/host"
            return False
        if pane:
            navigation["reader_open"] = True
            navigation["reader_target"] = target
            navigation["notice"] = "Opened on the right; Esc closes it"
        else:
            navigation["notice"] = "Right reader unavailable at this size/host"
        return bool(pane)

    def close_call():
        from terminal_panel import managed_session
        try:
            closed = managed_session() and panel_reader.close_reader(os.environ.get("TMUX_PANE", ""))
            navigation.pop("reader_open", None)
            navigation.pop("reader_target", None)
            return closed
        except (OSError, subprocess.SubprocessError):
            return False

    def close_detail():
        navigation.pop("detail", None)
        navigation.pop("transcript_back", None)

    def interrupted(number, unused):
        from terminal_panel import NativeShutdown
        for signum in saved_handlers:
            signal.signal(signum, signal.SIG_IGN)
        raise NativeShutdown(number)

    try:
        for name in ("SIGTERM", "SIGHUP"):
            number = getattr(signal, name, None)
            if number is not None:
                try:
                    saved_handlers[number] = signal.signal(number, interrupted)
                except (OSError, ValueError):
                    pass
        if capable:
            print("\033[?25l", end="", flush=True)
        with TerminalInput(managed_session(), active=capable) as inputs:
            # Native reads can defer Python's SIGINT handler; redirected watches need the input loop too.
            background = BackgroundRead() if inputs.fd is not None or not capable and not sys.stdout.isatty() else None
            previews = BackgroundRead()
            state, status, main_attempt = {"repo": {"name": repo.name}, "collection": {"ok": False, "pending": True}}, 0, None
            density, filtered = view or board_view("auto"), attention_only
            layout_mode = "work"
            next_read, next_frame, revision = 0, 0, 0
            held = {}
            next_focus, splits, modes = 0, SplitSync(), PanelModeSync()
            while True:
                collected = False
                if background:
                    if not background.pending and time.monotonic() >= next_read:
                        submitted_revision = revision
                        background.start(lambda: read_panel(repo, provider, view, attention_only))
                    collected, result = background.take()
                    if collected and isinstance(result, Exception):
                        if inputs.fd is None:
                            raise result
                        result = None
                else:
                    result = read_panel(repo, provider, view, attention_only)
                    collected = True
                if collected:
                    if result is None:
                        result = ({"repo": {"name": repo.name}, "collection": {"ok": False}}, 1,
                                  provider, main_attempt, density, filtered, os.environ.get("OMS_ROOM_ID", ""))
                    state, status, provider, main_attempt, read_density, read_filtered, selected_room = result
                    os.environ["OMS_ROOM_ID"] = selected_room
                    state, status, notice = hold_state(held, state, status, selected_room)
                    if view is not None or not managed_session():
                        read_density = navigation.get("view") or read_density
                    if "attention" in navigation:
                        # The `b` key overrides a flag or window option that would turn the filter back on.
                        read_filtered = navigation["attention"]
                    if not background or revision == submitted_revision:
                        density, filtered = read_density, read_filtered
                    pending_cancel = navigation.get("cancel_pending")
                    navigation = refreshed_navigation(navigation, state, managed_session() and session_option(BOTTOM_OPTION),
                                                     close_call=close_call)
                    if pending_cancel and not navigation.get("cancel_pending"):
                        _clear_cancel_option(pending_cancel.get("window_id"))
                    if navigation.get("cancel_pending"):
                        _poll_cancel_confirmation(repo, state, navigation, main_attempt)
                    stale_notice(navigation, notice)
                    if managed_session() and state.get("collection", {}).get("ok") is True and main_attempt:
                        alert = (main_needs_attention(state, main_attempt), main_has_open_questions(state, main_attempt))
                        if alert != flagged:
                            mark_window(*alert)
                            flagged = alert
                    if managed_session():
                        update_context_header(state, main_attempt, repo, navigation)
                    if n and not sys.stdout.isatty():
                        print("----")
                    n += 1
                    next_read = time.monotonic() + 5
                    if inputs.fd is not None and not count and reload_ready(stamps):
                        # The re-executed watcher keeps what the person selected, so a reload never moves it.
                        os.environ["OMS_PANEL_RESUME"] = json.dumps({key: navigation.get(key) for key in (
                            "room_id", "selected", "dismissed", "tab")} | {
                            "preview": (navigation.get("preview") or {}).get("target")})
                        raise ReloadWatcher()
                    if REJECTED_RELOAD.pop("notice", False):
                        navigation["notice"] = "New panel code does not start yet; keeping this board"

                if background and inputs.fd is None and not collected:
                    inputs.wait(.05)
                    continue

                def redraw(discard_stale_clicks=False):
                    from terminal_panel import _cancel_watch_authority, draw_frame, frame_view, main_occupied
                    from terminal_panel import managed_session
                    nonlocal last
                    drawn_hits = navigation.get("hits")
                    owner = next((member for member in __import__("room_view").nodes(state)
                                  if member.get("role") == "main"
                                  and main_attempt in (member.get("participant"), member.get("attempt"))), None)
                    navigation["cancel_allowed"] = bool(owner and _cancel_watch_authority(
                        repo, main_attempt, owner["participant"]))
                    navigation.update(hits=[], items=[], expanded=expanded, installed=installed,
                                      spawn_busy=any(entry[0].poll() is None for entry in spawns),
                                      spawn_managed=managed_session(),
                                      main_occupied=status != 0 or bool(navigation.get("capacity_stale")) or bool(main_occupied(state)) or any(m.get("role") == "main" and m.get("state") not in
                                          {"done", "failed", "cancelled", "timed_out", "abandoned"}
                                          for m in __import__("room_view").nodes(state)))
                    drawn, size, terminal = frame_view(repo, provider, state, menu=False,
                        main_attempt=main_attempt, frame=frame if motion else None,
                        view=density, attention_only=filtered, navigation=navigation)
                    if discard_stale_clicks and navigation.get("hits") != drawn_hits:
                        # Clear old-map clicks before the new frame can receive fresh ones.
                        inputs.discard_clicks()
                    last = draw_frame(drawn, size, terminal, last)
                    return size

                size = redraw(discard_stale_clicks=collected)
                if count and n >= count:
                    return status
                deadline = time.monotonic() + .05 if background and background.pending else next_read
                while True:
                    pending_cancel = navigation.get("cancel_pending")
                    if (pending_cancel and navigation.get("selected") !=
                            ("result", pending_cancel.get("call_id"))):
                        _clear_cancel_option(pending_cancel.get("window_id"))
                        navigation.pop("cancel_pending", None)
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        break
                    events = inputs.wait(min(.05 if previews.pending else
                                             .25 if motion or inputs.fd is not None else 5, remaining))
                    try:
                        resized = os.get_terminal_size(sys.stdout.fileno()) != size
                    except (OSError, ValueError):
                        resized = False
                    if managed_session() and inputs.fd is not None and time.monotonic() >= next_focus:
                        next_focus = time.monotonic() + 1
                        geometry = panel_geometry()
                        # Finish any edge change before Focus zooms the native pane.
                        splits.tick(geometry)
                        modes.tick(geometry)
                        current_mode = geometry[6] if geometry and len(geometry) > 6 else "work"
                        if current_mode != layout_mode:
                            layout_mode = current_mode
                            density = "graph" if layout_mode == "overview" else board_view("auto")
                            navigation["layout_mode"] = layout_mode
                            navigation["view"] = density
                            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                                        "@oms_panel_view", density), check=False, timeout=2,
                                           stdin=subprocess.DEVNULL)
                    if resized:
                        size = redraw()
                    batch_hits = navigation.get("hits")
                    changed_from = None
                    discard_batch = False
                    for event in events:
                        pending_cancel = navigation.get("cancel_pending")
                        if (pending_cancel and navigation.get("selected") !=
                                ("result", pending_cancel.get("call_id"))):
                            _clear_cancel_option(pending_cancel.get("window_id"))
                            navigation.pop("cancel_pending", None)
                        if pending_cancel and event[0] == "c":
                            _clear_cancel_option(pending_cancel.get("window_id"))
                            navigation.pop("cancel_pending", None)
                            navigation["notice"] = "Call cancellation not confirmed"
                            size = redraw()
                            continue
                        if discard_batch:
                            continue
                        publish_bottom(navigation, changed_from)
                        changed_from = bottom_state(navigation)
                        if event[0] == "click" and navigation.get("hits") != batch_hits:
                            continue  # The board was redrawn after this batch was read; the click names the old map.
                        if navigation.get("health_view") and event[0] not in {"k", "escape", "u", "repaint", "q", "eof"}:
                            continue
                        revision += 1
                        navigation.pop("notice", None)
                        armed = navigation.pop("spawn_armed", False)
                        swap_armed = navigation.pop("swap_armed", False)
                        spawn = None
                        if event[0] == "click" and not resized:
                            spawn = next((h["action"][1] for h in navigation.get("hits", []) if h["action"][0] == "spawn"
                                          and h["y"] == event[2] and h["x1"] <= event[1] <= h["x2"]), None)
                        elif armed and event[0] in {"1", "2", "0"}:
                            spawn = {"1": "claude", "2": "pi", "0": "codex"}[event[0]]
                        if event[0] == "s" and managed_session():
                            if not navigation.get("main_occupied"):
                                navigation["notice"] = "No main to swap"
                            else:
                                choices = swap_choices(repo, navigation.get("room_id"))
                                names = {"claude": "1", "pi": "2", "codex": "0"}
                                navigation["notice"] = "Swap main: " + "  ".join(
                                    "%s %s %s (%s weekly left)" % (names[c["provider"]], c["provider"], c["model"],
                                    "%s%%" % c["weekly_left"] if c["weekly_left"] is not None else "unknown")
                                    for c in choices)
                                navigation["swap_armed"] = True
                            size = redraw()
                            continue
                        if swap_armed and event[0] in {"1", "2", "0"}:
                            selected = {"1": "claude", "2": "pi", "0": "codex"}[event[0]]
                            if selected in installed:
                                start_spawn(repo, selected, spawns, navigation, swap=True)
                            else:
                                navigation["notice"] = "%s is not installed" % selected.capitalize()
                            size = redraw()
                            continue
                        if spawn or armed and event[0] == "escape" or event[0] == "n":
                            if spawn:
                                if navigation.get("main_occupied"):
                                    navigation["notice"] = "One main is already active; finish it before starting another"
                                elif spawn in installed and managed_session():
                                    start_spawn(repo, spawn, spawns, navigation)
                                else:
                                    navigation["notice"] = "%s is not installed or the tmux panel is not open" % spawn.capitalize()
                            elif event[0] == "n":
                                if navigation.get("main_occupied"):
                                    navigation["notice"] = "One main is already active; finish it before starting another"
                                else:
                                    navigation["spawn_armed"] = True
                                    navigation.pop("notice", None)
                            else:
                                navigation.pop("notice", None)
                            size = redraw()
                            continue
                        if event[0] == "eof" or event[0] == "q" and not managed_session():
                            return status
                        if resized and event[0] == "click":
                            continue
                        if event[0] in {"u", "repaint"}:
                            next_read, last = 0, None
                            navigation["notice"] = "Display refreshed; native sessions and retained history preserved"
                        elif event[0] == "k":
                            navigation["health_view"] = not navigation.get("health_view", False)
                        elif event[0] == "i" and managed_session():
                            toggle_context_headers()
                        elif event[0] == "m" and managed_session():
                            mode = cycle_panel_mode(os.environ.get("TMUX_PANE", ""))
                            if mode:
                                layout_mode = mode
                                density = "graph" if mode == "overview" else board_view("auto")
                                navigation["layout_mode"] = mode
                                navigation["view"] = density
                                subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                                            "@oms_panel_view", density), check=False, timeout=2,
                                               stdin=subprocess.DEVNULL)
                            else:
                                navigation["notice"] = "Layout mode unavailable; native session preserved"
                        elif event[0] == "escape" and close_call():
                            pass  # Esc first closes only the reader pane; the board keeps its preview.
                        elif event[0] == "escape" and navigation.get("health_view"):
                            navigation["health_view"] = False
                        elif tab_event(event, navigation):
                            if navigation.get("finalize_call"):
                                inputs.__exit__(None, None, None)
                                try:
                                    clear_control_view()
                                    navigation["notice"] = finalize_selection(repo, provider, navigation, state)
                                except (EOFError, KeyboardInterrupt):
                                    navigation.pop("finalize_call", None)
                                    navigation["notice"] = "Finalization cancelled"
                                except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                                    navigation["notice"] = clean(str(error), 160)
                                finally:
                                    inputs.__enter__()
                                    last, next_read = None, 0
                                size = redraw()
                                break  # Pending keys from the earlier frame cannot answer owner prompts.
                            target = navigation.pop("open_call", None)
                            if target:
                                try:
                                    if not open_call(target):
                                        navigate(repo, target, state, navigation, size.columns)
                                except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                                    navigation["notice"] = clean(str(error), 160)
                        elif (event[0] == "f" and navigation.get("tab") == "debate" and navigation.get("surface") in {"graph", "tree"}
                                and not navigation.get("detail") and (navigation.get("debate_view") or {}).get("shown")):
                            navigation["notice"] = "Opening recorded result..."
                            size = redraw()
                            try:
                                if not open_call(("debate", navigation["debate_view"]["shown"])):
                                    navigate(repo, ("debate", navigation["debate_view"]["shown"]), state, navigation, size.columns)
                            except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                                navigation["notice"] = clean(str(error), 160)
                        elif event[0] == "escape" and navigation.get("group_tree") and not navigation.get("detail"):
                            reset_group_view(navigation)
                            density = "graph"
                            navigation["view"] = density
                        elif event[0] == "escape" and navigation.get("keys_help"):
                            navigation["keys_help"] = False
                        elif event[0] == "escape":
                            navigation.pop("opening", None)
                            if not navigation.pop("detail", None):
                                navigation.pop("preview", None)
                                navigation["dismissed"] = True
                            navigation["offset"] = 0
                            try:
                                expand(False)
                            except (OSError, ValueError, subprocess.SubprocessError):
                                navigation["notice"] = "Board collapse unavailable; native session preserved"
                            if density in {"tree", "graph"}:
                                density = board_view("auto")
                            navigation["view"] = density
                            if managed_session():
                                subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                               "@oms_panel_view", density), check=False, timeout=2, stdin=subprocess.DEVNULL)
                        elif event[0] == "?":
                            navigation["keys_help"] = not navigation.get("keys_help", False)
                        elif event[0] == "w" and not navigation.get("detail"):
                            try:
                                navigation["notice"] = open_worktree(repo, state, navigation)
                            except (OSError, ValueError, subprocess.SubprocessError) as error:
                                navigation["notice"] = clean(str(error), 160)
                        elif event[0] == "a" and not navigation.get("detail"):
                            try:
                                navigation["notice"] = ask_advisor(repo, state, navigation)
                            except (OSError, ValueError, subprocess.SubprocessError) as error:
                                navigation["notice"] = clean(str(error), 160)
                        elif event[0] == "c" and not navigation.get("detail"):
                            message, pending = _request_cancel_confirmation(repo, state, navigation, main_attempt)
                            navigation["notice"] = message
                            if pending:
                                navigation["cancel_pending"] = pending
                                size = redraw()
                                discard_batch = True
                        elif event[0] in {"g", "t", "b", "v"}:
                            if event[0] in {"g", "t"}:
                                reset_group_view(navigation)
                            close_detail()
                            navigation["offset"] = 0
                            if event[0] == "b":
                                filtered = not filtered
                                navigation["attention"] = filtered
                            elif event[0] == "v":
                                try:
                                    expand(not expanded)
                                    density = "graph" if expanded else board_view("auto")
                                except (OSError, ValueError, subprocess.SubprocessError):
                                    navigation["notice"] = "Board expansion unavailable; native session preserved"
                            else:
                                density = "graph" if event[0] == "g" else "tree"
                            navigation["view"] = density
                            if managed_session():
                                name, value = (("@oms_panel_attention", "1" if filtered else "0")
                                               if event[0] == "b" else ("@oms_panel_view", density))
                                subprocess.run(tmux_command("set-option", "-w", "-t",
                                    os.environ.get("TMUX_PANE", ""), name, value),
                                    check=False, timeout=2, stdin=subprocess.DEVNULL)
                        else:
                            action = choose(("down",) if event[0] == "tab" and not navigation.get("tab_strip_visible", False) else event, navigation)
                            shown = None if action else panel_reader.picked(event, navigation)
                            if shown:
                                # A click or Enter previews in the board; the call's full content opens on the right.
                                close_call() if shown[0] == "chat" else open_call(shown)
                            if action and not navigation.get("detail"):
                                if action[0] in {"chat", "result", "debate", "mail"}:
                                    # Show the selection before tmux or evidence reads can block.
                                    navigation["notice"] = ("Opening chat..." if action[0] == "chat" else
                                                            "Opening recorded result...")
                                    size = redraw()
                                try:
                                    if action[0] == "chat":
                                        close_call()
                                    if action[0] == "chat" and expanded:
                                        expand(False)
                                        density = board_view("auto")
                                        navigation["view"] = density
                                        if managed_session():
                                            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                                                       "@oms_panel_view", density), check=False, timeout=2, stdin=subprocess.DEVNULL)
                                    if action[0] not in {"result", "debate", "mail"} or not open_call(action):
                                        navigate(repo, action, state, navigation, size.columns)
                                except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                                    navigation["notice"] = clean(str(error), 160)
                        size = redraw()
                        if event[0] in {"g", "t", "v", "b", "k", "escape", "?", "u", "repaint"}:
                            # Even an empty/loading hit map becomes invalid after a mode change.
                            batch_hits = None
                    publish_bottom(navigation, changed_from)
                    for popup in [p for p in navigation.get("popups", []) if p.poll() not in (None, 0)]:
                        navigation["notice"] = ("Advisor popup unavailable (exit %s; tmux 3.2+ has popups); "
                                                "use the control menu a Advisor" % popup.returncode)
                    navigation["popups"] = [p for p in navigation.get("popups", []) if p.poll() is None]
                    if reap_spawns(spawns, navigation):
                        next_read, size = 0, redraw()
                        break
                    loaded, report = previews.take()
                    if isinstance(report, Exception):
                        report = None
                    if (loaded and isinstance(report, tuple) and len(report) == 2 and
                            type(report[0]) is int and isinstance(report[1], tuple) and len(report[1]) == 3):
                        generation, activity_report = report
                        if accept_activity_report(navigation, generation, activity_report):
                            size = redraw()
                        # Result reads are versioned; never let an old completion reach target-only handlers.
                        loaded, report = False, None
                    preview = navigation.get("preview")
                    opening = navigation.get("opening")
                    if loaded and opening and report is None:
                        navigation.pop("opening")
                        navigation["notice"] = "Result evidence unavailable"
                        size = redraw()
                    elif loaded and report and opening and report[0] == opening:
                        finish_opening(navigation, report)
                        size = redraw()
                    elif loaded and report and preview and preview.get("target") == report[0]:
                        preview.update(report=report[1], error=report[2])
                        size = redraw()
                    reader = navigation.get("debate_view") or {}
                    if loaded and reader.get("shown") and report and report[0] == ("debate", reader["shown"]):
                        reader.update(report=report[1] or {}, error=report[2])
                        size = redraw()
                    if navigation.get("tab") == "debate" and reader.get("shown") and not previews.pending and (
                            "report" not in reader or reader.pop("refresh", False)):
                        target = ("debate", reader["shown"])

                        def debate_read(target=target):
                            from terminal_panel import call_results, clean
                            try:
                                return target, panel_debate.read(repo, target, call_results), None
                            except Exception as error:
                                return target, {}, clean(str(error), 120) or "Debate evidence unavailable"
                        previews.start(debate_read)
                    log = navigation.setdefault("messages_view", {"selected": None, "open": None, "filter": 0, "scroll": 0})
                    if loaded and report and report[0][0] == "messages":
                        log.update(log=report[1] or log.get("log"), rev=report[0][1], reading=False)
                        size = redraw()
                    if navigation.get("tab") == "messages" and state and not previews.pending and \
                            log.get("rev") != panel_messages.signature(state):
                        stale = panel_messages.signature(state)
                        room_id = stale[0]

                        def messages_read(stale=stale, room_id=room_id):
                            from terminal_panel import clean
                            try:
                                return ("messages", stale), panel_messages.read(repo, room_id), None
                            except Exception as error:
                                return ("messages", stale), None, clean(str(error), 120)
                        if room_id:
                            log["reading"] = True
                            previews.start(messages_read)
                    if navigation.get("opening") and not previews.pending:
                        target = navigation["opening"]
                        if target[0] == "result":
                            generation = navigation.get("opening_generation")
                            previews.start(lambda target=target, state=state, generation=generation:
                                           (generation, read_evidence(repo, target, state)))
                        else:
                            previews.start(lambda target=target, state=state:
                                           read_evidence(repo, target, state))
                    elif preview and preview["target"][0] == "chat":
                        # Main detail is drawn from the room snapshot; no evidence read is needed.
                        preview.setdefault("report", {})
                        preview.pop("refresh", None)
                    elif preview and ("report" not in preview or preview.pop("refresh", False)) and not previews.pending:
                        target = preview["target"]
                        if target[0] == "result":
                            generation = preview.get("generation")
                            if type(generation) is not int:
                                generation = navigation.get("activity_generation", 0)
                                if type(generation) is not int:
                                    generation = 0
                                preview["generation"] = generation
                                navigation["activity_generation"] = generation
                            previews.start(lambda target=target, state=state, generation=generation:
                                           (generation, read_evidence(repo, target, state)))
                        else:
                            previews.start(lambda target=target, state=state:
                                           read_evidence(repo, target, state))
                    if expire_notice(navigation, time.monotonic()):
                        size = redraw()
                    if motion and time.monotonic() >= next_frame:
                        frame += 1
                        size = redraw()
                        next_frame = time.monotonic() + .25
    finally:
        if flagged and any(flagged):
            mark_window(False)
        if expanded:
            try:
                expand(False)
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
        if sys.exc_info()[0] is not ReloadWatcher:
            close_call()  # a re-executed watcher keeps its pane and finds its reader again
        for number, previous in saved_handlers.items():
            signal.signal(number, previous)
        if capable:
            print("\033[?25h\033[0m", flush=True)
