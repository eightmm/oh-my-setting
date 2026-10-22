"""The reader pane: one recorded call's full result right of the fixed left board, above the native chat."""

import json
import os
import re
import runpy
import shutil
import subprocess
import sys
import threading
import time
from datetime import datetime
from pathlib import Path

import room
from dashboard_projection import clean
from panel_input import TerminalInput, hit_at
from panel_results import ID

OPTION = "@oms_panel_reader"  # pane option on a reader: the board pane that owns it
BOARD = "OMS_PANEL_READER_BOARD"  # set only in a reader's command; its --results then pages
PANE = re.compile(r"%[0-9]+\Z")
SHARE, MIN_ROWS, MIN_CHAT = 62, 8, 5
VIEWS = ("Summary", "Raw", "Events", "Diff", "Run info")
READER_VIEWS = VIEWS + ("Transcript",)
PATCH_LIMIT = 65536
EVENT_TAIL_BYTES = 1024 * 1024
SGR = re.compile(r"\x1b\[[0-9;]*m")


class ReaderText(str):
    def __new__(cls, value, artifact=None, updated_at=None):
        result = str.__new__(cls, value)
        result.artifact = artifact
        result.updated_at = updated_at
        result.trusted_style = True
        return result


def styled_clip(line, width):
    """Fit trusted colour spans while keeping all other terminal controls inert."""
    from dashboard_projection import display_width
    from panel_view import clipped
    out, used, styled = [], 0, False
    for piece in re.split(r"(\x1b\[[0-9;]*m)", str(line)):
        if SGR.fullmatch(piece):
            out.append(piece)
            styled = piece != "\033[0m"
        elif piece:
            fitted = clipped(piece, max(0, width - used))
            out.append(fitted)
            used += display_width(fitted)
        if used >= width:
            break
    if styled:
        out.append("\033[0m")
    return "".join(out)


def event_tail(repo, attempt_id, maximum=EVENT_TAIL_BYTES):
    """Read a bounded tail of lifecycle events through the ledger's safe descriptor."""
    maximum = max(1, min(int(maximum), EVENT_TAIL_BYTES))
    source = runpy.run_path(str(Path(__file__).with_name("agent-events.py")))
    path = source["event_path"](Path(repo))
    try:
        with source["_ledger_fd"](path) as descriptor:
            size = os.fstat(descriptor).st_size
            start = max(0, size - maximum)
            boundary = False
            if start:
                os.lseek(descriptor, start - 1, os.SEEK_SET)
                boundary = os.read(descriptor, 1) == b"\n"
            os.lseek(descriptor, start, os.SEEK_SET)
            chunks, remaining = [], size - start
            while remaining:
                chunk = os.read(descriptor, min(65536, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
    except FileNotFoundError:
        return [], False
    data = b"".join(chunks)
    if start and not boundary:
        first = data.find(b"\n")
        data = data[first + 1:] if first >= 0 else b""
    raw_lines = data.splitlines()
    if data and not data.endswith(b"\n") and raw_lines:
        raw_lines.pop()
    rows = []
    for line in raw_lines:
        try:
            row = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as error:
            raise ValueError("lifecycle event tail is invalid") from error
        if isinstance(row, dict) and row.get("attempt_id") == attempt_id:
            rows.append(row)
    rows_truncated = len(rows) > 300
    rows = rows[-300:]
    started = any(row.get("event_type") == "attempt.created" for row in rows)
    return rows, bool(not started and (start or rows_truncated))


def render_view(view, report, call, events=(), patch=None, width=100, timeline_truncated=False,
                patch_error=False, heading=None, color=None, transcript=()):
    """Render one bounded, read-only view for a recorded call."""
    from dashboard_projection import display_width
    from panel_view import (call_identity, clipped, light_theme, paint, role_style, status_tag,
                            wrapped, _result_blocks, _result_structured)
    from room_view import readable
    color = sys.stdout.isatty() if color is None else color
    colors = 256 if "256color" in os.environ.get("TERM", "") else 16
    light = light_theme()
    def ink(value, style_name, bold=False):
        return paint(value, style_name, color=color, colors=colors, light=light, bold=bold)
    selected = READER_VIEWS.index(view)
    labels = [("[" + name + "]" if i == selected else name) for i, name in enumerate(READER_VIEWS)]
    tabs = "  ".join(ink(name, "metadata", bold=i == selected)
                      for i, name in enumerate(labels))
    if display_width("  ".join(labels)) > width:
        tabs = clipped("Tab: [" + view + "]", width)
    row = next(iter(report.get("rows", [])), {})
    model = row.get("model") or call.get("served_model") or call.get("selected_model") or call.get("requested_model")
    purpose = row.get("purpose") or call.get("purpose")
    identity = (call_identity(row.get("role") or call.get("role") or "worker", model,
                              row.get("title") or row.get("task_id"), alias=call.get("alias"), purpose=purpose)
                if purpose else heading or call_identity(row.get("role") or call.get("role") or "worker", model,
                                                         row.get("title") or row.get("task_id"), alias=call.get("alias")))
    title = ink(clipped(identity, width), role_style(row.get("role"), row.get("purpose")), bold=True)
    state = status_tag(row.get("process_state") or call.get("process_state") or call.get("state") or "unknown")
    tone = ("running" if any(word in state for word in ("running", "starting", "verifying")) else
            "bad" if any(word in state for word in ("failed", "timed out", "lost process")) else
            "alert" if any(word in state for word in ("blocked", "needs approval", "awaiting decision")) else
            "dim")
    chrome = [tabs, title, ink(state, tone)]
    lines = []
    if view == "Transcript":
        for line in transcript or ["transcript not captured"]:
            lines.extend(_result_structured(line, max(8, width)))
        return "\n".join(chrome + lines)
    if view == "Summary":
        if not row:
            lines.extend(("No result recorded yet", "Run info: no result or attempt row was retained."))
        else:
            if call.get("answer_truncated"):
                lines.append("[partial answer: retained preview is incomplete]")
            checks = (row.get("verification") or {}).get("status") or (
                "not run" if call.get("verify_exit") is None else
                "passed: exit 0" if call.get("verify_exit") == 0 else
                "failed: exit %s" % call.get("verify_exit"))
            verdict = call.get("review_verdict") or "not recorded"
            outcome = row.get("outcome") or "pending"
            lines.extend(("Task: " + str(row.get("task_id") or "unknown"),
                          "Execution: " + state,
                          "Checks: " + str(checks).replace("_", " "),
                          "Review: " + str(verdict),
                          "Main decision: " + str(outcome)))
            if call.get("ts"):
                lines.append("Recorded: " + clean(str(call["ts"]), 80))
            # Failed calls can have lifecycle metadata but no answer artifact. Keep the
            # reason and recovery path visible in the reader instead of an empty result.
            if not (call.get("answer") or row.get("summary")):
                ending = call.get("ending") or call.get("state") or row.get("process_state")
                if ending:
                    lines.append("Ending: " + clean(str(ending).replace("_", " "), 160))
                for error in (call.get("error"), call.get("error_lines"), row.get("issue")):
                    if error:
                        values = error if isinstance(error, (list, tuple)) else str(error).splitlines()
                        lines.extend("Error: " + clean(str(value), 240) for value in values if str(value).strip())
                location = call.get("worktree") or call.get("location") or row.get("location")
                if location:
                    lines.append("Kept worktree: " + clean(str(location), 500))
                action = row.get("next_action") or call.get("next_action")
                if location and row.get("task_id"):
                    action = "oms panel --salvage " + str(row["task_id"])
                elif ending:
                    action = action or "inspect Run info and retry or finalize the task"
                if action:
                    lines.append("Next: " + clean(str(action), 240))
            answer = call.get("answer") or row.get("summary")
            if outcome == "pending" and (call.get("answer") or row.get("summary")):
                lines.append("Next: check the answer" + (" and patch" if call.get("patch") else "") +
                             ", then decide")
            lines.append("")
            title = "Findings" if row.get("role") == "reviewer" else (
                "Recommendation" if row.get("role") in ("advisor", "researcher") else "What changed")
            lines.append(title)
            if answer:
                sections = re.split(r"(?m)^\s*#{0,3}\s*(?:What changed|Answer|Recommendation|Findings|Remaining uncertainty)\s*:?\s*$",
                                    str(answer), flags=re.I)
                excerpt = next((part.strip() for part in sections if part.strip()), "")
                lines.extend(wrapped(readable(excerpt, paragraphs=True), max(8, width), 12))
                limit = re.search(r"(?ims)^\s*#{0,3}\s*Remaining uncertainty\s*:?\s*$\n(.*?)(?=^\s*#{1,3}\s|\Z)",
                                  str(answer))
                if limit:
                    lines.extend(("", "Author-stated limits"))
                    lines.extend(wrapped(readable(limit.group(1), paragraphs=True), max(8, width), 8))
            else:
                lines.append("No result recorded yet")
            changes = call.get("changes") or {}
            if changes.get("count") is not None:
                lines.extend(("", "Changed files: %s (+%s -%s)" % (
                    changes.get("count"), changes.get("added", "?"), changes.get("removed", "?"))))
    elif view == "Raw":
        answer = call.get("answer") or row.get("summary")
        if not answer:
            lines.append("No result recorded yet")
        else:
            if call.get("answer_truncated"):
                lines.append("[partial answer: retained preview is incomplete]")
            for source, kind in _result_blocks(str(answer)):
                if kind in ("code", "table"):
                    lines.extend(_result_structured(source, max(8, width)))
                elif re.match(r"^\s{0,3}#{1,6}\s", source):
                    lines.append(ink(clipped(clean(source, 2000), width), "metadata", bold=True))
                elif re.match(r"^\s*(?:[-*+]\s|\d+[.)]\s)", source):
                    lines.extend("  " + part for part in wrapped(clean(source, 2000), max(8, width - 2), 100))
                else:
                    lines.extend(wrapped(clean(source, 2000), max(8, width), 100) if source.strip() else [""])
    elif view == "Run info":
        attempts = [str(item.get("attempt_id")) for item in (row.get("calls") or [])
                    if isinstance(item, dict) and item.get("attempt_id")]
        for label, value in (("Provider", row.get("provider") or call.get("provider")),
                             ("Model", model), ("Effort", row.get("effort") or call.get("effort")),
                             ("Task", row.get("task_id")), ("Attempt", call.get("attempt_id")),
                             ("Attempts", ", ".join(attempts)),
                             ("Answer artifact", call.get("artifact")), ("Patch", call.get("patch")),
                             ("Patch digest", call.get("patch_sha256")), ("Owner result", row.get("artifact"))):
            lines.extend(wrapped(label + ": " + str(value or "not recorded"), max(8, width), 100))
        for label, value in (("Ending", (call.get("ending") or call.get("state") or row.get("process_state") or "")
                             .replace("_", " ")),
                             ("Termination reason", row.get("termination_reason")),
                             ("Error", call.get("error") or call.get("error_lines") or row.get("issue")),
                             ("Kept worktree", call.get("worktree") or call.get("location") or row.get("location"))):
            if value:
                values = value if isinstance(value, (list, tuple)) else str(value).splitlines()
                for item in values:
                    if str(item).strip():
                        lines.extend(wrapped(label + ": " + clean(str(item), 500), max(8, width), 100))
        for stage, detail in (row.get("stages") or {}).items():
            if isinstance(detail, dict):
                lines.extend(wrapped("Stage %s: %s" % (stage.replace("_", " "), detail.get("state", "not_run")),
                                     max(8, width), 100))
        if row.get("representative_state"):
            lines.extend(wrapped("Next action: " + str(row.get("next_action") or "none"), max(8, width), 100))
        for evidence in (row.get("evidence") or []):
            if isinstance(evidence, dict) and evidence.get("path"):
                lines.extend(wrapped("Evidence: " + clean(str(evidence["path"]), 2000), max(8, width), 100))
    if view in ("Summary", "Raw", "Run info"):
        return "\n".join(chrome + lines)
    if view == "Events":
        def event_order(row):
            stamp = row.get("ts") or row.get("updated_at") or ""
            try:
                stamp = datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()
            except (AttributeError, OSError, OverflowError, TypeError, ValueError):
                stamp = 0
            return stamp, row.get("seq", 0)

        rows = sorted(events, key=event_order)
        heartbeats = 0
        for row in rows:
            kind = str(row.get("event_type", row.get("type", "")))
            if "heartbeat" in kind:
                heartbeats += 1
                continue
            if heartbeats:
                lines.append("  %d heartbeats" % heartbeats)
                heartbeats = 0
            state = row.get("kind") or row.get("to_state") or row.get("state") or row.get("event_type") or kind
            state = {"attempt.created": "started", "attempt.state_changed": "settled"}.get(state, state)
            stamp = row.get("ts") or row.get("updated_at") or "time unavailable"
            try:
                local = datetime.fromisoformat(stamp.replace("Z", "+00:00")).astimezone()
                stamp = local.strftime("%H:%M:%S" if local.date() == datetime.now().astimezone().date()
                                       else "%m-%d %H:%M:%S")
            except (AttributeError, TypeError, ValueError):
                pass
            stamp = clean(str(stamp), 40)
            label = clean(str(state), 120)
            detail = row.get("verdict") or row.get("status") or row.get("outcome")
            summary = row.get("summary")
            if isinstance(detail, dict):
                detail = detail.get("status") or detail.get("outcome")
            if row.get("kind") == "review" and detail in ("passed", "failed"):
                detail = "GATE not recorded"
            if summary:
                label += " · " + clean(str(summary), 240)
            elif row.get("kind") in ("review", "patch-admit", "patch-land", "panel-delivery", "panel-delivery-pending") and not detail:
                label += " · recorded"
            actor = row.get("actor") or row.get("participant") or row.get("role")
            actor_tag = "[" + clean(str(actor), 40) + "]" if actor else ""
            prefix = "%s  %s  %s" % (stamp, actor_tag, label)
            outcome = "[" + clean(str(detail), 120) + "]" if detail else ""
            row_width = max(8, min(200, width))
            outcome = clipped(outcome, row_width) if outcome else ""
            if outcome and display_width(prefix) + display_width(outcome) + 2 <= row_width:
                parts = [prefix + " " * (row_width - display_width(prefix) - display_width(outcome)) + outcome]
            else:
                parts = wrapped(prefix, row_width, 100, max_chars=400)
                if outcome:
                    parts.append(" " * max(0, row_width - display_width(outcome)) + outcome)
            for part in parts:
                lines.append(part.replace(actor_tag, ink(actor_tag, role_style(row.get("role") or actor)), 1)
                             if actor_tag else part)
        if timeline_truncated:
            lines.extend(wrapped("[timeline truncated: call start is outside the retained event window]",
                                 max(8, min(200, width)), 100, max_chars=400))
        if heartbeats:
            lines.append("  %d heartbeats" % heartbeats)
        if not lines:
            lines.append("No retained events")
        return "\n".join(chrome + lines)
    if not patch:
        lines.append("[✗ unavailable] patch unreadable (digest mismatch / read error)" if patch_error else "No patch recorded")
        return "\n".join(chrome + lines)
    content = patch[:PATCH_LIMIT].decode("utf-8", "replace")
    for line in content.splitlines():
        line = clipped(line, max(8, width))
        if line.startswith(("diff --git ", "--- ", "+++ ", "@@ ")):
            lines.append(ink(line, "metadata", bold=True))
        elif line.startswith("+"):
            lines.append(ink(line, "good"))
        elif line.startswith("-"):
            lines.append(ink(line, "bad"))
        else:
            lines.append(line)
    if len(patch) > PATCH_LIMIT:
        lines.append("[patch capped at %d bytes]" % PATCH_LIMIT)
    return "\n".join(chrome + lines)


def tmux(*args):
    return subprocess.run(["tmux"] + list(args), capture_output=True, text=True, check=False, timeout=3,
                          stdin=subprocess.DEVNULL)


def target_args(target):
    """Read-only `oms panel` arguments; identifiers from recorded state are revalidated here."""
    kind, ident = target
    if kind not in ("result", "debate", "mail"):
        raise ValueError("only recorded results or main messages open in the reader")
    task, who = ident if kind == "debate" else (None, ident)
    room.identifier(who, "participant")
    if task is not None and (not isinstance(task, str) or not ID.fullmatch(task)):
        raise ValueError("invalid task ID")
    if kind == "mail":
        return ["--mail", who]
    return ["--results", "--participant", who] + (["--task-id", task] if task else [])


def picked(event, navigation):
    """The call or main a click or Enter just chose on the board (it previews there), else None."""
    if navigation.get("surface") == "graph":
        return None
    target = (navigation.get("preview") or {}).get("target")
    if event[0] == "click":
        chosen = (hit_at(event, navigation) or {}).get("action")
    else:
        chosen = navigation.get("selected") if event[0] == "enter" else None
    return target if target and chosen == target and target[0] in ("result", "debate", "chat") else None


def split_rows(height):
    """Reader rows taken from a chat pane `height` rows tall; None when the reader or the chat would be too small."""
    rows = height * SHARE // 100
    return rows if rows >= MIN_ROWS and height - rows - 1 >= MIN_CHAT else None


def window_panes(board):
    """{pane: (height, owning board)} for the board's window."""
    result = tmux("list-panes", "-t", board, "-F", "#{pane_id}\t#{pane_height}\t#{" + OPTION + "}")
    panes = {}
    for line in result.stdout.replace("\r", "").splitlines() if result.returncode == 0 else []:
        fields = line.split("\t")
        if len(fields) == 3 and PANE.fullmatch(fields[0]) and fields[1].isdigit():
            panes[fields[0]] = (int(fields[1]), fields[2])
    return panes


def find(board, panes=None):
    panes = window_panes(board) if panes is None else panes
    return next((pane for pane, (unused, owner) in panes.items() if owner == board and pane != board), None)


def open_reader(board, command, cwd):
    """Run `command` in the board's reader pane and return the pane: an existing reader is respawned, otherwise the
    native chat pane is split with the reader on top. None when there is no recorded chat pane or too little room;
    the chat pane itself is never respawned or killed."""
    if not PANE.fullmatch(board or ""):
        return None
    panes = window_panes(board)
    pane = find(board, panes)
    if pane:
        return None if tmux("respawn-pane", "-k", "-t", pane, "-c", str(cwd), command).returncode else pane
    native = tmux("show-option", "-w", "-v", "-t", board, "@oms_panel_native_pane").stdout.strip()
    if not PANE.fullmatch(native) or native == board or native not in panes or not split_rows(panes[native][0]):
        return None
    made = tmux("split-window", "-d", "-v", "-b", "-l", str(split_rows(panes[native][0])), "-t", native,
                "-c", str(cwd), "-P", "-F", "#{pane_id}", command)
    pane = made.stdout.strip()
    if made.returncode or not PANE.fullmatch(pane):
        return None
    marked = False
    try:
        marked = tmux("set-option", "-p", "-t", pane, OPTION, board).returncode == 0
    finally:
        if not marked:
            try:
                tmux("kill-pane", "-t", pane)
            except (OSError, subprocess.SubprocessError):
                pass
    return pane if marked else None


def close_reader(board):
    """Close the board's reader pane; True when one was open."""
    pane = find(board) if PANE.fullmatch(board or "") else None
    if pane:
        tmux("kill-pane", "-t", pane)
    return bool(pane)


def page(render, board, on_refresh=None):
    """Scroll `render(width)` until q, Esc or the end of input, or until the board pane is gone (a reader never
    outlives its board); focus then returns to the board."""
    offset, shown, lines, drawn, alive = 0, None, ["[⟳ loading] Reading recorded result…"], None, time.monotonic() + 2
    view, previous_view = "Summary", "Summary"
    refresh, read = 0, {}
    title, state, tabs = "Call result", "[⟳ loading]", "  ".join(
        "[" + name + "]" if name == view else name for name in READER_VIEWS)
    updated, notice, query, matches, full_lines, searching = None, "", None, [], None, False
    artifact, source_time, zoomed, trusted_style = None, None, False, False

    def plain(value):
        return re.sub(r"\033\[[0-9;]*m", "", value)

    def sections():
        return [i for i, line in enumerate(lines) if re.match(r"^\s*(?:#{1,6}\s|[A-Z][\w /-]{2,}:?\s*$)", plain(line))]

    def find_matches():
        return [i for i, line in enumerate(lines) if query and query.casefold() in plain(line).casefold()]

    def fetch(width, requested_view):
        # One read at a time off the input thread: a slow result query never blocks q/Esc or scrolling.
        try:
            if on_refresh is not None:
                on_refresh()
            try:
                value = render(width, requested_view)
            except TypeError:
                value = render(width)
            read["lines"] = (width, requested_view, value.splitlines(), None,
                             getattr(value, "artifact", None), getattr(value, "updated_at", None),
                             getattr(value, "trusted_style", False) is True)
        except Exception as error:  # the pane stays open to say why instead of vanishing
            read["lines"] = (width, requested_view, ["[✗ unavailable] " + clean(str(error), 200)], True,
                             None, None, False)

    try:
        with TerminalInput(extra_keys=tuple("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789[]/.-_")) as inputs:
            while True:
                size = tuple(shutil.get_terminal_size())
                running = read.get("thread") and read["thread"].is_alive()
                if not running and (shown != size[0] or time.monotonic() >= refresh):
                    if shown == size[0] and updated is not None:
                        notice = "Updating…"
                    read["thread"] = threading.Thread(target=fetch, args=(size[0], view), daemon=True)
                    read["thread"].start()
                    read["thread"].join(.2)
                    shown, refresh = size[0], time.monotonic() + 2
                if "lines" in read:
                    width, fetched_view, content, error, next_artifact, next_time, next_style = read.pop("lines")
                    if width == size[0] and fetched_view == view:
                        if error and updated is not None:
                            notice = content[0] + " · showing earlier content"
                        else:
                            points = [i for i in sections() if i <= offset]
                            anchor = plain(lines[points[-1]]).strip() if points else None
                            within = offset - points[-1] if points else 0
                            if content and (plain(content[0]).startswith("Tab: [") or
                                    plain(content[0]).startswith(("[Summary]  ", "Summary  ")) and
                                    "Events" in plain(content[0]) and "Diff" in plain(content[0])):
                                tabs = content[0]
                                title = content[1] if len(content) > 1 else "Call result"
                                state = content[2] if len(content) > 2 else "[⟳ starting]"
                                content = content[3:]
                            lines, notice, updated, full_lines = content or ["No result recorded yet"], "", time.monotonic(), None
                            if next_artifact != artifact and artifact is not None:
                                offset = 0
                            elif anchor and next_artifact == artifact:
                                matching = next((i for i, line in enumerate(lines) if plain(line).strip() == anchor), None)
                                if matching is not None:
                                    offset = matching + within
                            artifact, source_time, trusted_style = next_artifact, next_time, next_style
                            matches = find_matches()
                        drawn = None
                    elif fetched_view != view:
                        shown = None
                compact = size[1] < 8
                height = max(1, size[1] - (1 if compact else 6))
                body_height = max(0, height - (1 if notice else 0))
                offset = max(0, min(offset, len(lines) - max(1, body_height)))
                if drawn != (offset, size, len(lines), notice, view, query):
                    from panel_view import clipped
                    body = ([clipped(notice, size[0])] if notice else []) + lines[offset:offset + body_height]
                    body += [""] * (height - len(body))
                    seconds = None
                    if source_time:
                        try:
                            stamp = datetime.fromisoformat(str(source_time).replace("Z", "+00:00")).astimezone()
                            seconds = max(0, (datetime.now().astimezone() - stamp).total_seconds())
                        except (TypeError, ValueError, OverflowError):
                            pass
                    age = ("unknown" if seconds is None else "<1m" if seconds < 60 else
                           "%dm" % (int(seconds // 60)) if seconds < 3600 else
                           "%dh" % (int(seconds // 3600)) if seconds < 86400 else "%dd" % (int(seconds // 86400)))
                    count = "rows %d-%d/%d" % (offset + 1, min(len(lines), offset + body_height), len(lines))
                    age_text = "updated " + age
                    footer = count + " " * max(2, size[0] - len(count) - len(age_text)) + age_text
                    if compact:
                        frame = body + [footer]
                    else:
                        frame = [title, state, tabs, ""] + body + [footer]
                        # Keep a single bottom region; the key hint is part of it.
                        hint = ("Tab View  r Raw  e Events  d Diff  i " + ("Info" if size[0] < 120 else "Run info") +
                                "  x Transcript  [ ] Section  o Outline  "
                                "/ Search  n/N Match  " + ("z Zoom  " if size[0] < 120 or zoomed else "") + "q/Esc close")
                        if len(hint) > size[0]:
                            hint = "q/Esc close  Tab View  x Transcript  / Search"
                        frame.append(hint)
                    fit_line = styled_clip if trusted_style else clipped
                    sys.stdout.write("\033[H\033[2J" + "\n".join(fit_line(line, size[0]) for line in frame[:size[1]]))
                    sys.stdout.flush()
                    drawn = (offset, size, len(lines), notice, view, query)
                for event in inputs.wait(1):
                    kind = event[0]
                    if searching:
                        if kind == "enter":
                            searching = False
                            matches = find_matches()
                            offset = matches[0] if matches else offset
                            notice = "%d matches" % len(matches)
                        elif kind == "escape":
                            searching, notice = False, ""
                        elif len(kind) == 1 and kind != "tab":
                            query += kind
                            notice = "Search: " + query
                        drawn = None
                        continue
                    if kind == "escape" and view == "Transcript":
                        view = previous_view
                        lines, offset, shown, drawn, updated = ["Reading recorded result…"], 0, None, None, None
                        continue
                    if kind in ("q", "escape", "eof"):
                        return 0
                    if kind == "repaint":
                        shown = drawn = None
                    if kind in ("tab", "r", "e", "d", "i", "x"):
                        if view != "Transcript":
                            previous_view = view
                        view = {"r": "Raw", "e": "Events", "d": "Diff", "i": "Run info", "x": "Transcript"}.get(
                            kind, READER_VIEWS[(READER_VIEWS.index(view) + 1) % len(READER_VIEWS)])
                        tabs = "  ".join("[" + name + "]" if name == view else name for name in READER_VIEWS)
                        lines, offset, shown, drawn, updated, notice, full_lines = (
                            ["[⟳ loading] Reading recorded result…"], 0, None, None, None, "", None)
                    if kind in ("[", "]"):
                        points = sections()
                        offset = (next((i for i in reversed(points) if i < offset), 0) if kind == "[" else
                                  next((i for i in points if i > offset), offset))
                    if kind == "o":
                        if full_lines is not None:
                            lines, full_lines = full_lines, None
                        else:
                            points = sections()
                            full_lines = lines
                            lines = ["Outline"] + ["%d. %s" % (i + 1, plain(lines[i]).strip()) for i in points] if points else ["No sections"]
                        offset, drawn = 0, None
                    if kind == "/":
                        query, searching = "", True
                        notice = "Search: type a term, Enter to finish"
                    if kind in ("n", "N") and matches:
                        offset = (next((i for i in matches if i > offset), matches[0]) if kind == "n" else
                                  next((i for i in reversed(matches) if i < offset), matches[-1]))
                    if kind == "z" and (size[0] < 120 or zoomed) and PANE.fullmatch(os.environ.get("TMUX_PANE", "")):
                        if tmux("resize-pane", "-Z", "-t", os.environ["TMUX_PANE"]).returncode == 0:
                            zoomed = not zoomed
                            drawn = None
                    offset = (0 if kind == "home" else len(lines) if kind == "end" else offset + (
                        event[1] if kind == "scroll" else {"up": -1, "down": 1, "pageup": -height, "pagedown": height,
                                                           " ": height}.get(kind, 0)))
                if time.monotonic() >= alive:
                    if tmux("display-message", "-p", "-t", board, "#{pane_id}").stdout.strip() != board:
                        return 0
                    alive = time.monotonic() + 2
    finally:
        tmux("select-pane", "-t", board)
