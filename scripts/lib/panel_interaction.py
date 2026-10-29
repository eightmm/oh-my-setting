"""Interactive terminal input, browsing and control-menu presentation."""

from pathlib import Path
import json
import os
import panel_debate
import panel_host
import panel_messages
import panel_reader
import re
import room
import shutil
import subprocess
import sys
import time
import unicodedata

# Call-local facade imports preserve the public entry point and avoid import cycles.


def read_field(label, required=True):
    from terminal_panel import MAX_PROMPT
    text = input(label).strip()
    if len(text.encode("utf-8")) > MAX_PROMPT:
        raise ValueError("input exceeds 64 KiB")
    if required and not text:
        raise ValueError("input is required")
    if any(ord(char) < 32 for char in text):
        raise ValueError("input must be a single printable line")
    return text


def browse_results(repo):
    from terminal_panel import clear_control_view, read_field, render_results, results
    while True:
        report = results(repo)
        width = shutil.get_terminal_size().columns
        clear_control_view()
        print(render_results(report, width, compact=True))
        rows = report.get("rows", [])
        choice = read_field("Result number (Enter to return): " if rows else "Enter to return: ", required=False)
        if not choice:
            return
        if len(choice) > 3 or not choice.isascii() or not choice.isdigit() or not 1 <= int(choice) <= len(rows):
            print("Choose a listed result number.")
            continue
        task_id = rows[int(choice) - 1]["task_id"]
        clear_control_view()
        print(render_results(results(repo, task_id), width))
        read_field("Press Enter to return to the result list: ", required=False)


def browse_chats(repo):
    from terminal_panel import clean, clear_control_view, read_field, render_results, results
    import panel_chats
    ident = os.environ.get("OMS_ROOM_ID")
    if not ident:
        raise ValueError("select an existing room first")
    while True:
        report = panel_chats.catalog(repo, ident)
        clear_control_view()
        print(panel_chats.text(report))
        choice = read_field("Chat number (a<number> for Codex app, Enter to return): ", required=False)
        if not choice:
            return
        app = choice.startswith("a")
        number = choice[1:] if app else choice
        if not number.isascii() or not number.isdigit() or not 1 <= int(number) <= len(report["rows"]):
            print("Choose a listed chat number.")
            continue
        row = report["rows"][int(number) - 1]
        if app and (row["provider"] != "codex" or row["role"] != "main" or not row["uri"]):
            print("Codex app navigation requires an enrolled native Codex main.")
        elif row["status"] == "artifacts":
            evidence = results(repo, room_participant=row["participant"])
            evidence["rows"] = [r for r in evidence["rows"] if r.get("calls")]
            print(render_results(evidence, shutil.get_terminal_size().columns))
        else:
            try:
                request = panel_chats.open_chat(repo, ident, row["participant"], surface="app" if app else "auto")
                print("Chat navigation requested: " + request["method"])
            except ValueError as error:
                print(clean(str(error), 180))
                if row.get("session_id"):
                    hint = "Claude app sidebar" if row["provider"] == "claude" else "Codex app"
                    print("Native session: " + row["session_id"] + " / choose it in the " + hint)
        read_field("Press Enter to return to chats: ", required=False)


def raw_keys():
    from terminal_panel import terminal_style
    return terminal_style()[0] and os.name == "posix" and sys.stdin.isatty()


GOAL_LIMIT = 4000  # Claude Code's /goal condition limit
_GOAL_HEAD = ("Work the OMS shared plan for this repository. Met when `oms agent-plan list` shows every task done%s, "
              "with %s shown in this conversation. Also met when the next step needs the user's "
              "decision, authority you do not have, or a hard blocker; state which and stop. Goal: ")


def _one_line(text):
    return " ".join("".join(" " if unicodedata.category(c).startswith("C") else c for c in str(text)).split())


def goal_condition(goal, has_accept=False):
    """The /goal condition for the shared plan; only the goal text is shortened to fit the limit.

    The stored acceptance command is run by `oms agent-plan accept` (which refuses a plan without one), never copied."""
    head = _GOAL_HEAD % ((" and `oms agent-plan accept` passes", "both outputs") if has_accept else ("", "its output"))
    room_left = GOAL_LIMIT - len(head)
    goal = _one_line(goal)
    return head + (goal if len(goal) <= room_left else goal[:room_left - 1].rstrip() + "\u2026")


def plan_goal(repo):
    """(goal, has_accept) from the stored plan's own fields; the board's report keeps neither in full."""
    from terminal_panel import _read
    plan = json.loads(_read(repo, ".oms/plan/tasks.json", 4 * 1024 * 1024))
    if not isinstance(plan, dict):
        raise ValueError("plan is unreadable")
    goal, accept = plan.get("goal"), plan.get("accept")
    return (_one_line(goal) if isinstance(goal, str) else "",
            isinstance(accept, str) and bool(accept.strip()))


def help_rows(rows, width, unicode=True):
    """menu_rows plus the goal-drive key, placed before the closing edge of the expanded box."""
    from panel_view import box_row
    return rows[:-1] + [box_row("GOAL".ljust(9) + "[d] Drive goal", width, unicode)] + rows[-1:]


def drive_goal(repo, state, harness, view="auto", attention_only=False):
    """Send the plan goal to the panel's Claude main as /goal after the person confirms; returns a status line."""
    from room_view import _plan_finished, plan_summary
    from terminal_panel import _lock, _swap_submit, main_occupied, managed_session, open_native, panel_room, read_field
    if not plan_summary(state):
        return "no active plan goal to drive"
    if _plan_finished(state):
        return "plan finished; retire or init the next goal first"
    goal, has_accept = plan_goal(repo)
    if not goal:
        return "no active plan goal to drive"
    live = main_occupied(state)
    if live and not managed_session():
        return "goal drive needs a tmux panel to type into the live main"
    if not live and harness != "claude":
        return "goal drive needs a Claude main; %s has no verified /goal" % harness
    condition = goal_condition(goal, has_accept)
    print("/goal " + condition)
    answer = read_field("Enter to send / type replacement text / n to cancel: ", required=False)
    if answer == "n":
        return "goal drive cancelled"
    if answer:
        condition = _one_line(answer)[:GOAL_LIMIT]
    if live:
        from panel_native import live_main_pane
        try:
            # swap_main verifies and types under this lock; so must a goal, or the two could interleave their keys.
            with _lock(repo, "panel-main-start", timeout=.1):
                pane, live_harness = live_main_pane(repo)
                if live_harness != "claude":
                    return "goal drive needs a Claude main; the live main is %s" % live_harness
                _swap_submit(pane, "/goal " + condition)
        except ValueError as error:
            if "busy" in str(error):
                return "main start or swap in progress; retry"
            raise
        return "goal sent to claude"
    os.environ["OMS_ROOM_ID"] = panel_room(repo, title="Plan goal")
    status = open_native("claude", repo, task="/goal " + condition, view=view, attention_only=attention_only)
    return "goal sent to claude (new main) / exit=%s" % status


def menu_input():
    from terminal_panel import managed_session
    from panel_input import TerminalInput

    class MenuInput(TerminalInput):
        """The board's input model plus every printable key, so the control menu needs no Enter."""

        def decode(self, data):
            events, index = [], 0
            while index < len(data):
                byte = data[index]
                sequence = (re.match(rb"\x1b(?:\[[0-9;<]*[A-Za-z~]|O.)", data[index:])
                            if byte == 27 and not self.pending else None)
                if not self.pending and 33 <= byte < 127:
                    events.append((chr(byte),))
                    index += 1
                elif sequence:
                    events += super().decode(sequence.group())
                    index += sequence.end()
                else:
                    events += super().decode(data[index:index + 1] if byte != 27 else data[index:])
                    index = len(data) if byte == 27 else index + 1
            return events
    return MenuInput(managed_session())


def read_choice(repo, provider, previous, view, attention_only, navigation, cache):
    """One control key without Enter; arrows, clicks and Enter act on the Needs-you list. A line elsewhere."""
    from terminal_panel import clean, finish_opening, managed_session, menu_input, navigate, read_evidence
    from terminal_panel import read_field, reader_shell, tab_event
    from panel_input import choose, passive_click

    def repaint():
        from terminal_panel import _show_state, call_results, clean, show
        state, unused = _show_state(repo, navigation=navigation, cache=cache)
        reader = navigation.get("debate_view") or {}
        if navigation.get("tab") == "debate" and reader.get("shown") and (
                "report" not in reader or reader.pop("refresh", False)):
            try:
                reader.update(report=panel_debate.read(repo, ("debate", reader["shown"]), call_results), error=None)
            except Exception as error:
                reader.update(report=None, error=clean(str(error), 120) or "Debate evidence unavailable")
        log = navigation.setdefault("messages_view", {"selected": None, "open": None, "filter": 0, "scroll": 0})
        if navigation.get("tab") == "messages" and state and log.get("rev") != panel_messages.signature(state):
            signature = panel_messages.signature(state)
            room_id = signature[0]
            if room_id:
                try:
                    messages = panel_messages.read(repo, room_id)
                except Exception as error:
                    log.update(error=clean(str(error), 120), rev=signature, reading=False)
                else:
                    log.update(log=messages or log.get("log"), rev=signature, reading=False, error=None)
        show(repo, provider, previous, view=view, attention_only=attention_only, navigation=navigation, cache=cache)

    # Raw mode starts before the first frame, so keys typed while it loads are kept.
    with menu_input() as inputs:
        repaint()
        if inputs.fd is None:
            choice = read_field("oms> ")
            if navigation.get("detail"):
                navigation.pop("detail", None)
                navigation.pop("transcript_back", None)
                navigation["offset"] = 0
                if choice == "q":
                    return "9"
            return "9" if choice == "u" else choice
        deadline = time.monotonic() + 5
        queued = navigation.pop("queued", [])
        while True:
            events, queued = queued or inputs.wait(max(0, min(.25, deadline - time.monotonic()))), []
            for position, event in enumerate(events):
                if event[0] == "repaint":
                    return "9"
                if event[0] == "eof":
                    raise EOFError
                # Match the watch loop: refresh and health keys stay board controls,
                # while graph dock tabs/readers get first chance at their own input.
                if (event[0] not in {"u", "k"} and navigation.get("surface") in {"graph", "tree"}
                        and tab_event(event, navigation)):
                    if navigation.get("finalize_call"):
                        navigation["queued"] = events[position + 1:]
                        return "f"
                    target = navigation.pop("open_call", None)
                    if target:
                        navigation.pop("transcript_back", None)
                        width = max(1, shutil.get_terminal_size().columns)
                        navigate(repo, target, cache.get("state", {}), navigation, width)
                        if navigation.get("opening"):
                            finish_opening(navigation, read_evidence(repo, navigation["opening"]))
                    repaint()
                    continue
                if (event[0] == "f" and navigation.get("surface") in {"graph", "tree"}
                        and navigation.get("tab", "detail") == "detail" and not navigation.get("detail")
                        and (navigation.get("full_mail") or not navigation.get("full_result"))):
                    target = choose(event, navigation)
                    if target:
                        try:
                            board = os.environ.get("TMUX_PANE", "")
                            pane = (managed_session() and not panel_host.herdr_active() and panel_reader.open_reader(
                                board, reader_shell(repo, target, board, navigation.get("room_id")), repo))
                            if pane:
                                navigation["reader_open"] = True
                                navigation["reader_target"] = target
                                navigation["notice"] = "Opened on the right; Esc closes it"
                            else:
                                navigate(repo, target, cache.get("state", {}), navigation,
                                         max(1, shutil.get_terminal_size().columns))
                                navigation["notice"] = "Right reader unavailable at this size/host"
                        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                            navigation["notice"] = "Right reader unavailable at this size/host: " + clean(str(error), 120)
                        repaint()
                    continue
                if len(event) == 1 and len(event[0]) == 1 and event[0] != " ":
                    # Keys typed ahead run on the next read, not lost with this one.
                    navigation["queued"] = events[position + 1:]
                    if navigation.get("detail"):
                        if event[0] == "q":
                            navigation.pop("detail", None)
                            navigation.pop("transcript_back", None)
                            navigation["offset"] = 0
                            return "9"
                        navigation.pop("detail", None)
                        navigation.pop("transcript_back", None)
                        navigation["offset"] = 0
                    return "9" if event[0] == "u" else event[0]
                if event[0] == "escape":
                    if navigation.get("reader_open") and managed_session():
                        try:
                            panel_reader.close_reader(os.environ.get("TMUX_PANE", ""))
                        except (OSError, subprocess.SubprocessError) as error:
                            navigation["notice"] = clean(str(error), 160)
                        navigation.pop("reader_open", None)
                        repaint()
                        continue
                    if navigation.pop("detail", None) is None:
                        continue
                    navigation["offset"] = 0
                    repaint()
                    continue
                navigation.pop("notice", None)
                if (navigation.get("detail") and event[0] in ("enter", "click")) or passive_click(event, navigation):
                    continue  # Ignored input; repainting would only flicker text the person is selecting.
                action = choose(event, navigation)
                if action:
                    navigation.pop("transcript_back", None)
                    width = max(1, shutil.get_terminal_size().columns)
                    navigate(repo, action, cache.get("state", {}), navigation, width)
                    if navigation.get("opening"):
                        # No watch loop reads in the background here; read now so the row opens.
                        finish_opening(navigation, read_evidence(repo, navigation["opening"]))
                repaint()
            navigation.pop("queued", None)
            if time.monotonic() >= deadline:
                stamps = navigation.get("reload_stamps")
                if (stamps is not None and not events and not navigation.get("detail")
                        and not navigation.get("reader_open") and not navigation.get("finalize_call")):
                    from terminal_panel import ReloadWatcher, reload_ready
                    if reload_ready(stamps):
                        raise ReloadWatcher()  # menu_input restores the terminal; terminal_panel re-executes
                return "9"


def interactive(repo, view="auto", attention_only=False):
    from terminal_panel import ENTRY, MAIN_HARNESSES, MAX_PROMPT, NativeShutdown, SESSION_ID, _adopt_successor
    from terminal_panel import browse_chats, browse_results, clean, clear_control_view, council, dispatch
    from terminal_panel import ensure_room, finalize_selection, hold_state, live_main_refusal, main_installed
    from terminal_panel import main_occupied, main_owner, managed_session, menu_rows, native_binary
    from terminal_panel import open_native, panel_mains, panel_room, private_capacity, provider_catalog
    from terminal_panel import raw_keys, read_choice, read_field, recorded_room, reopen_panel, retry_delivery
    from terminal_panel import room_listing, room_open, run, safe_label, show_providers, snapshot, source_stamps
    from terminal_panel import stale_notice, swap_choices, swap_main, terminal_style, tmux_command
    harness = os.environ.get("OMS_PANEL_HARNESS") or (panel_mains() or ["claude"])[0]
    provider = os.environ.get("OMS_PANEL_OWNER") or main_owner(harness)
    previous = "ready"
    navigation = {}
    held = {}
    next_room_poll = 0
    stamps = source_stamps()
    while True:
        if managed_session() and time.monotonic() >= next_room_poll:
            next_room_poll = time.monotonic() + 5
            try:
                adopted = recorded_room(os.environ["OMS_PANEL_SESSION"], checked=True)
                if not adopted:
                    os.environ.pop("OMS_ROOM_ID", None)
                    raise ValueError("session room is unset")
                if adopted and room.resolve(repo, adopted) != adopted:
                    # The session's room rolled over: record the successor for every window, then follow it.
                    _adopt_successor(repo)
                    adopted = room.resolve(repo, adopted)
                if adopted and not room_open(repo, adopted):
                    raise ValueError("selected room is unavailable or closed")
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                navigation["room_poll_unknown"] = True
                previous = "Room selection unknown: " + clean("%s: %s" % (type(error).__name__, error), 100)
            else:
                navigation.pop("room_poll_unknown", None)
                if adopted != os.environ.get("OMS_ROOM_ID"):
                    if adopted:
                        os.environ["OMS_ROOM_ID"] = adopted
                    else:
                        os.environ.pop("OMS_ROOM_ID", None)
                    navigation.clear()
        if managed_session():
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_owner", provider), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_view", view), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_room", os.environ.get("OMS_ROOM_ID", "")), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_attention", "1" if attention_only else "0"), check=True)
        cache = {"held": held}
        if managed_session():
            old_notice = navigation.pop("main_notice", None)
            if old_notice is not None and navigation.get("notice") == old_notice:
                navigation.pop("notice", None)
            notice = subprocess.run(tmux_command("show-options", "-qv", "-t", "=" + os.environ["OMS_PANEL_SESSION"],
                                                 "@oms_panel_notice"), capture_output=True, text=True,
                                    check=False, timeout=3, stdin=subprocess.DEVNULL)
            if notice.returncode == 0 and notice.stdout.strip() and "notice" not in navigation:
                navigation["notice"] = navigation["main_notice"] = clean(notice.stdout.replace("\r", "").strip(), 200)
        navigation["installed"] = [name for name in MAIN_HARNESSES if main_installed(name)]
        navigation["line_input"] = not raw_keys()
        navigation["reload_stamps"] = stamps
        try:
            choice = read_choice(repo, provider, previous, view, attention_only, navigation, cache)
            if choice == "u":
                choice = "9"
            if choice in ("1", "2", "0", "3", "t") and (
                    cache.get("status", 1) != 0 or navigation.get("capacity_stale") or navigation.get("room_poll_unknown")
                    or cache.get("state", {}).get("capacity_stale") or main_occupied(cache.get("state", {}))
                    or cache.get("state", {}).get("collection", {}).get("ok") is not True
                    or cache.get("state", {}).get("room", {}).get("error")):
                occupied = main_occupied(cache.get("state", {}))
                raise ValueError("Room selection unknown; wait for a successful room poll" if navigation.get("room_poll_unknown")
                                 else live_main_refusal(repo, [occupied] if occupied else None))
            if choice != "9":
                # Keys typed ahead only follow a refresh; any other choice may open a text prompt.
                navigation.pop("queued", None)
            if choice == "z":
                report = reopen_panel(repo)
                previous = "panel " + report["status"] + "; input session preserved"
                continue
            if choice == "?":
                width = max(1, shutil.get_terminal_size().columns)
                clear_control_view()
                style = terminal_style()[2]
                print("\n".join(help_rows(menu_rows(width, 40, style, expanded=True, managed=managed_session(), occupied=navigation.get("main_occupied", True), installed=navigation.get("installed")), width, style)))
                read_field("Press Enter to return to the panel: ", required=False)
                continue
            if choice == "k":
                from panel_metrics import collect
                import panel_health
                import panel_release
                from panel_cache import TickReads
                while True:
                    report = dict(cache.get("state", {}))
                    report["capacity_stale"] = bool(report.get("capacity_stale") or navigation.get("capacity_stale"))
                    report["provider_status"] = collect(report, repo=repo)
                    report["private_capacity"] = private_capacity(repo)
                    report["dead_mains"] = panel_release.dead_mains(repo, _tick=TickReads(repo))
                    size = shutil.get_terminal_size((100, 28))
                    clear_control_view()
                    print(panel_health.render(report, size.columns, size.lines))
                    if read_field("Enter to return / 9 Refresh: ", required=False) != "9":
                        break
                    state, status = snapshot(repo)
                    state, status, notice = hold_state(held, state, status, os.environ.get("OMS_ROOM_ID", ""))
                    stale_notice(navigation, notice)
                    cache.update(state=state, status=status)
                continue
            if choice == "q":
                session = os.environ.get("OMS_PANEL_SESSION", "")
                if managed_session():
                    subprocess.run(tmux_command("detach-client", "-s", session), check=True)
                    previous = "detached; existing sessions preserved"
                    continue
                return 0
            if choice == "j":
                pane = os.environ.get("TMUX_PANE", "")
                if not os.environ.get("TMUX") or not re.fullmatch(r"%[0-9]+", pane):
                    raise ValueError("terminals and jobs require a tmux pane")
                result = subprocess.run(tmux_command("display-message", "-p", "-t", pane, "#{session_id}"),
                                        capture_output=True, text=True, check=False, timeout=3,
                                        stdin=subprocess.DEVNULL)
                origin_session = result.stdout.replace("\r", "").strip()
                if result.returncode or not re.fullmatch(r"\$[0-9]+", origin_session):
                    raise ValueError("could not prove the current tmux session")
                import panel_workspaces
                panel_workspaces.open_workspace(repo, origin_session)
                previous = "terminals and jobs opened in a separate workspace"
                continue
            if choice == "s":
                choices = swap_choices(repo, os.environ.get("OMS_ROOM_ID"))
                if not choices:
                    raise ValueError("no installed main providers are available")
                clear_control_view()
                for choice_row in choices:
                    left = ("%s%%" % choice_row["weekly_left"] if choice_row["weekly_left"] is not None else "unknown")
                    print("%s  %s  weekly left %s" % (choice_row["provider"], choice_row["model"], left))
                selected = read_field("Swap to provider (Enter to cancel): ", required=False)
                if not selected:
                    continue
                if selected not in {row["provider"] for row in choices}:
                    raise ValueError("select an installed main provider")
                requested = read_field("Exact model (Enter for displayed model): ", required=False) or None
                print("Waiting for handoff; the current main and workers remain visible on the board.", flush=True)
                result = swap_main(repo, selected, requested)
                previous = "Swapped to %s %s in the same room" % (result["provider"], result["model"])
                continue
            if choice in ("1", "2", "0", "3"):
                selected = {"1": "claude", "2": "pi", "0": "codex"}.get(choice) or read_field("Harness (claude/pi/codex or opted-in native adapter): ")
                if selected not in MAIN_HARNESSES:
                    selected = provider_catalog(selected)[0]["provider"]
                    native_binary(selected)
                resume = read_field("Exact session ID: ") if choice == "3" else None
                if resume and not re.fullmatch(SESSION_ID, resume):
                    raise ValueError("invalid session ID")
                harness, provider = selected, main_owner(selected)
                os.environ["OMS_ROOM_ID"] = panel_room(repo)
                status = open_native(harness, repo, resume, view=view, attention_only=attention_only)
                if os.environ.get("TMUX") and os.environ.get("OMS_PANEL_SESSION") and status == 0:
                    previous = "%s window opened; native outcome unreported" % provider
                else:
                    previous = "%s native session exit=%s" % (provider, status)
            elif choice == "4":
                selected = read_field("Main harness (claude/pi/codex): ")
                if selected not in MAIN_HARNESSES:
                    raise ValueError("select claude, pi or codex")
                harness, provider = selected, main_owner(selected)
                previous = "main provider selected"
            elif choice == "t":
                task = read_field("Task for the main: ")
                try:
                    title = safe_label(task) or "Shared work"
                except ValueError:
                    title = "Shared work"
                os.environ["OMS_ROOM_ID"] = panel_room(repo, title=title)
                status = open_native(harness, repo, task=task, view=view, attention_only=attention_only)
                previous = "%s task opened / exit=%s" % (provider, status)
            elif choice in ("5", "6", "7", "a", "p", "e"):
                target = model = effort = None
                explicit = choice == "p"
                if explicit:
                    show_providers(provider_catalog())
                    target = read_field("Target provider (Enter to return): ", required=False)
                    if not target:
                        continue
                    choice = read_field("Action (n Native / 5 Explain / 6 Implement / 7 Review / a Advisor): ")
                    if choice == "n":
                        selected = provider_catalog(target)[0]["provider"]
                        model = read_field("Native model (Enter for profile): ", required=False) or None
                        task = read_field("Native task (Enter for readiness): ", required=False) or None
                        os.environ["OMS_ROOM_ID"] = panel_room(repo)
                        status = open_native(selected, repo, model=model, task=task,
                                             view=view, attention_only=attention_only)
                        previous = "%s native exit=%s" % (selected, status)
                        continue
                    if choice not in ("5", "6", "7", "a"):
                        raise ValueError("choose a listed peer action")
                    model = read_field("Exact model (Enter only for profile-controlled CLIs): ", required=False) or None
                    effort = read_field("Effort (low/medium/high, Enter for native settings): ", required=False) or None
                    if effort not in (None, "low", "medium", "high"):
                        raise ValueError("choose a supported effort level")
                action = {"5": "ask", "6": "delegate", "7": "review", "a": "advisor", "e": "research"}[choice]
                prompt = brief = verify = None
                if action == "delegate":
                    brief = Path(read_field("Scoped brief file: ")).expanduser()
                    if not brief.is_absolute():
                        brief = repo / brief
                    brief = brief.resolve(strict=True)
                    if not brief.is_file() or brief.stat().st_size > MAX_PROMPT:
                        raise ValueError("brief must be a regular file of at most 64 KiB")
                else:
                    prompt = read_field("Question / review task: ")
                if action in ("delegate", "review"):
                    verify = read_field("Verification command: ")
                role = "researcher" if choice == "e" else "advisor" if choice == "a" else "reviewer" if choice == "7" else "worker"
                purpose = {"5": "explain", "6": "implement"}.get(choice)
                workload = (read_field("Tier (light/routine/deep/frontier) [role default]: ", required=False) or None
                            if role != "researcher" and not explicit else None)
                status = dispatch(repo, provider, role, workload, "write" if choice == "6" else "read",
                                  purpose, prompt, brief, verify, target=target, model=model, effort=effort)
                previous = "%s %s exit=%s (not acceptance)" % (provider, role, status)
                read_field("Press Enter to return to the panel: ", required=False)
            elif choice == "8":
                run(["bash", str(ENTRY), "dashboard", "--repo", str(repo)], repo)
                read_field("Press Enter to return: ", required=False)
            elif choice == "r":
                browse_results(repo)
            elif choice == "h":
                browse_chats(repo)
            elif choice == "o":
                try:
                    rooms, incomplete = room.discover(repo)
                    width = max(1, shutil.get_terminal_size().columns)
                    from panel_view import LIVE_STATES, clipped
                    attempts = cache.get("state", {}).get("attempts", {})
                    live = {ident for row in attempts.get("active_recent", []) + attempts.get("recent", [])
                            if (row.get("panel") or {}).get("role") == "main" and
                            row.get("state") in LIVE_STATES | {"review", "waiting_input", "waiting_approval"}
                            for ident in (row.get("attempt_id"), (row.get("panel") or {}).get("room_participant"))
                            if ident}
                    clear_control_view()
                    for line in room_listing(rooms, os.environ.get("OMS_ROOM_ID"), width, live_participants=live):
                        print(line)
                    if incomplete:
                        print(clipped("Room scan incomplete; some rooms are unknown", width))
                    choice_room = read_field("Room number / n New / Enter Cancel: ", required=False)
                    if not choice_room:
                        continue
                    if choice_room == "n":
                        ident = room.create(repo, title=read_field("Room title: "))
                    elif choice_room.isdigit() and 1 <= int(choice_room) <= len(rooms):
                        ident = ensure_room(repo, rooms[int(choice_room) - 1]["id"])
                    else:
                        raise ValueError("choose a listed room number or n")
                    if managed_session():
                        subprocess.run(tmux_command("set-environment", "-t", "=" + os.environ["OMS_PANEL_SESSION"],
                                                    "OMS_ROOM_ID", ident), check=True, timeout=3, stdin=subprocess.DEVNULL)
                    os.environ["OMS_ROOM_ID"] = ident
                except SystemExit as error:
                    previous = "room selection failed: exit %s" % error.code
                    continue
                previous = "room selected: " + os.environ["OMS_ROOM_ID"]
                navigation.clear()
            elif choice == "g":
                view = "graph" if view != "graph" else "auto"
                navigation["offset"] = 0
                previous = "view: " + view
            elif choice == "c":
                task_id = read_field("Discussion task ID: ")
                label = read_field("Task title: ")
                prompt = read_field("Question for the council: ")
                workload = read_field("Difficulty routine/deep/frontier [deep]: ", required=False) or "deep"
                if workload not in ("routine", "deep", "frontier"):
                    print("Unknown difficulty; using deep.")
                    workload = "deep"
                status = council(repo, provider, task_id, label, prompt, workload=workload)
                previous = "council exit=%s / owner decision pending" % status
                read_field("Press Enter to return: ", required=False)
            elif choice == "d":
                previous = drive_goal(repo, cache.get("state", {}), harness, view, attention_only)
            elif choice == "f":
                previous = finalize_selection(repo, provider, navigation, cache.get("state", {}))
            elif choice == "n":
                task_id = read_field("Task ID to retry saved delivery (may duplicate an uncertain send): ")
                delivered = retry_delivery(repo, task_id)
                previous = "app delivery: " + delivered["receipt"]["status"]
            elif choice != "9":
                available = help_rows(menu_rows(200, 100, expanded=True, managed=managed_session(),
                                                occupied=navigation.get("main_occupied", True), installed=navigation.get("installed")), 200)
                raise ValueError("choose " + ", ".join(re.findall(r"\[([^]]+)\]", " ".join(available))))
        except EOFError:
            return 0
        except NativeShutdown:
            raise
        except KeyboardInterrupt:
            previous = "interrupted; inspect the recorded attempt before retrying"
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            message = str(error)
            if message.startswith("this panel already has a live main or unverified owner;"):
                lines = message.splitlines()
                previous = "error: " + "\n".join(
                    clean(line, len(line) if line.startswith("oms panel --release-main ") else 147)
                    for line in lines)
            else:
                previous = "error: " + clean(message, 140)
