"""Board navigation, reader selection, headers and shared view state."""

from pathlib import Path
import json
import os
import panel_debate
import panel_messages
import panel_reader
import re
import room
import shlex
import subprocess
import time
import uuid

# Call-local facade imports preserve the public entry point and avoid import cycles.


def _cancel_window_id():
    from terminal_panel import managed_session, tmux_command
    pane = os.environ.get("TMUX_PANE", "")
    if not managed_session() or not re.fullmatch(r"%[0-9]+", pane):
        return None
    try:
        result = subprocess.run(tmux_command("display-message", "-p", "-t", pane, "#{window_id}"),
                                capture_output=True, text=True, check=False, timeout=2,
                                stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    window_id = result.stdout.replace("\r", "").strip()
    return window_id if result.returncode == 0 and re.fullmatch(r"@[0-9]+", window_id) else None


def _cancel_client_name(window_id):
    from terminal_panel import tmux_command
    session = os.environ.get("OMS_PANEL_SESSION", "")
    if not re.fullmatch(r"@[0-9]+", window_id or "") or not session:
        return None
    try:
        result = subprocess.run(tmux_command("list-clients", "-t", "=" + session,
                                             "-F", "#{client_name} #{window_id}"),
                                capture_output=True, text=True, check=False, timeout=2,
                                stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    matches = []
    for row in result.stdout.replace("\r", "").splitlines():
        fields = row.split()
        if len(fields) == 2 and fields[1] == window_id:
            matches.append(fields[0])
    return matches[0] if len(matches) == 1 else None


def _clear_cancel_option(window_id):
    from terminal_panel import tmux_command
    if not re.fullmatch(r"@[0-9]+", window_id or ""):
        return
    try:
        subprocess.run(tmux_command("set-option", "-w", "-u", "-t", window_id,
                                    "@oms_cancel_confirm"), check=False, timeout=2,
                       stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        pass


def _begin_cancel_confirmation(member, attempt, client_name=None, window_id=None):
    from terminal_panel import _cancel_window_id, clean, tmux_command
    window_id = window_id or _cancel_window_id()
    if not window_id or not client_name:
        return None
    nonce = uuid.uuid4().hex
    role = clean(member.get("role") or "call", 32)
    model = clean(member.get("model") or "unknown model", 48)
    task = clean(attempt.get("task_id") or member.get("label") or member.get("participant"), 64)
    prompt = ("Cancel %s %s %s? (y/n)" % (role, model, task)).replace("#", "##")
    command = "set-option -w -t %s @oms_cancel_confirm %s" % (window_id, nonce)
    try:
        result = subprocess.run(tmux_command("confirm-before", "-b", "-t", client_name,
                                             "-p", prompt, command),
                                capture_output=True, text=True, check=False, timeout=2,
                                stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    return {"call_id": member["participant"], "attempt_id": attempt["attempt_id"],
            "nonce": nonce, "expires": time.monotonic() + 15, "window_id": window_id}


def _cancel_option_value(window_id):
    from terminal_panel import tmux_command
    try:
        result = subprocess.run(tmux_command("show-option", "-w", "-v", "-t", window_id,
                                             "@oms_cancel_confirm"), capture_output=True,
                                text=True, check=False, timeout=1, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    return result.stdout.replace("\r", "").strip()


def _poll_cancel_confirmation(repo, state, navigation, main_attempt, now=None):
    from terminal_panel import _cancel_option_value, _clear_cancel_option, cancel_selected_call
    pending = navigation.get("cancel_pending")
    if not isinstance(pending, dict):
        return None
    now = time.monotonic() if now is None else now
    selected = navigation.get("selected")
    attempt = ((state.get("room_attempts") or {}).get(pending.get("call_id")) or {})
    if (selected != ("result", pending.get("call_id"))
            or attempt.get("attempt_id") != pending.get("attempt_id")):
        _clear_cancel_option(pending.get("window_id"))
        navigation.pop("cancel_pending", None)
        return None
    if now >= pending.get("expires", 0):
        _clear_cancel_option(pending.get("window_id"))
        navigation.pop("cancel_pending", None)
        navigation["notice"] = "Call cancellation confirmation expired"
        return navigation["notice"]
    value = _cancel_option_value(pending.get("window_id"))
    if value is None:
        return None
    _clear_cancel_option(pending.get("window_id"))
    navigation.pop("cancel_pending", None)
    # The nonce is visible in tmux command arguments and a window option. A same-UID
    # process with tmux-server access can observe or forge it; this guards accidental
    # keys and pasted input, not hostile same-UID code.
    if value != pending.get("nonce"):
        navigation["notice"] = "Call cancellation not confirmed"
        return navigation["notice"]
    navigation["notice"] = cancel_selected_call(repo, state, navigation, main_attempt, send=True)
    return navigation["notice"]


def _request_cancel_confirmation(repo, state, navigation, main_attempt):
    from terminal_panel import _begin_cancel_confirmation, _cancel_client_name, _cancel_window_id
    from terminal_panel import cancel_selected_call, managed_session
    if not managed_session():
        return "Cancel needs the managed tmux panel", None
    result = cancel_selected_call(repo, state, navigation, main_attempt)
    if result != "ready":
        return result, None
    call_id = navigation["selected"][1]
    attempt = (state.get("room_attempts") or {}).get(call_id) or {}
    member = next((row for row in (state.get("room", {}).get("participants") or [])
                   if row.get("participant") == call_id), {})
    window_id = _cancel_window_id()
    client_name = _cancel_client_name(window_id)
    if not client_name:
        return "Cancel needs a single attached client", None
    pending = _begin_cancel_confirmation(member, attempt, client_name, window_id)
    if not pending:
        return "Call cancellation confirmation unavailable", None
    return "Confirm cancellation in the tmux prompt", pending


def session_option(name):
    """A user option of this panel's tmux session ("" when unset or unknown)."""
    from terminal_panel import tmux_command
    session = os.environ.get("OMS_PANEL_SESSION", "")
    if not session:
        return ""
    try:
        # Option commands take a target-pane; "=name:" is the exact-session form.
        result = subprocess.run(tmux_command("show-options", "-v", "-t", "=" + session + ":", name),
                                capture_output=True, text=True, check=False, timeout=5, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return ""
    return result.stdout.strip() if result.returncode == 0 else ""


def pane_option(name):
    from terminal_panel import tmux_command
    try:
        result = subprocess.run(tmux_command("show-option", "-w", "-v", "-t",
                                os.environ.get("TMUX_PANE", ""), name),
                                capture_output=True, text=True, check=False, timeout=5)
        return result.stdout.strip() if result.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def pane_local_option(name):
    from terminal_panel import tmux_command
    try:
        result = subprocess.run(tmux_command("show-option", "-p", "-v", "-t",
                                os.environ.get("TMUX_PANE", ""), name),
                                capture_output=True, text=True, check=False, timeout=5)
        return result.stdout.strip() if result.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def reset_group_view(navigation):
    """Leave graph inspection without touching the room or native session."""
    if navigation.pop("group_tree", None):
        navigation.pop("opening", None)
    navigation.pop("expanded_groups", None)
    navigation.pop("geometry", None)
    if (navigation.get("selected") or (None,))[0] == "group":
        navigation.pop("selected", None)
    navigation["offset"] = 0


def navigate(repo, action, state, navigation, width):
    from terminal_panel import clean, managed_session, pane_option
    kind, ident = action
    if kind == "group":
        if action not in navigation.get("items", []):
            return
        expanded = navigation.setdefault("expanded_groups", set())
        expanded.discard(ident) if ident in expanded else expanded.add(ident)
        if navigation.get("surface") == "graph":
            navigation["group_tree"] = True
        navigation["offset"] = 0
        navigation.pop("geometry", None)
        return
    if kind == "fold":
        folded = navigation.setdefault("collapsed", set())
        folded.discard(ident) if ident in folded else folded.add(ident)
        return
    room_id = navigation.get("room_id")
    if not room_id or managed_session() and not room.same_room(repo, pane_option("@oms_panel_room"), room_id):
        raise ValueError("room selection changed; refresh before navigating")
    current = room.status(repo, room_id)
    if kind in {"chat", "result", "debate", "mail"}:
        who = ident[1] if kind == "debate" else ident
        member = next((m for m in current["participants"] if m["participant"] == who and m["joined"]), None)
        if member is None or (kind in {"chat", "debate", "mail"}) != (member["role"] == "main"):
            raise ValueError("participant left or its role changed; refresh before navigating")
    if kind == "chat":
        import panel_chats
        opened = panel_chats.open_chat(repo, room_id, ident,
            allowed_methods={"existing-terminal", "app-uri", "windows-uri"})
        navigation["notice"] = "Chat navigation requested " + {"existing-terminal": "in its window", "app-uri": "in the app",
                                                                  "windows-uri": "in the app"}.get(opened["method"], "")
    elif kind == "mail":
        current = dict(current, messages=panel_messages.read(repo, room_id))
        navigation["detail"] = {"title": "MESSAGES", "text": panel_messages.mail_page(current, ident, width)}
        navigation["offset"] = 0
    elif kind in {"result", "debate"}:
        # The watch loop reads the evidence in the background and opens the detail when it arrives.
        navigation["opening"] = action
        navigation["notice"] = "Opening recorded result..."
    elif kind == "task":
        task = next((t for t in state.get("room", {}).get("repo_tasks", []) if t.get("id") == ident), None)
        if task is None:
            raise ValueError("task is outside the displayed snapshot")
        lines = ["Task snapshot: " + clean(task.get("id")), clean(task.get("title")),
                 "State: " + clean(task.get("state"))]
        depends = task.get("depends") or []
        if depends:
            lines.append("Depends: " + ", ".join(clean(item) for item in depends))
        # Plan records name the claiming provider; the lease itself stays out of the panel.
        claimant = task.get("provider") if task.get("claimed_at") or task.get("state") in ("claimed", "running") else None
        if claimant:
            from panel_view import age_label
            age = age_label(task.get("claimed_at")) if task.get("claimed_at") else ""
            lines.append("Claimed by: " + clean(claimant) + (" / " + age + " ago" if age else ""))
        verify = task.get("verify")
        if verify:
            lines.append("Verify: " + clean(verify, 240))
        if task.get("id") in state.get("completed_tasks", {}):
            lines += ["Work: completed locally; passing owner verification recorded.",
                      "Plan acceptance: " + clean(task.get("state"))]
        lines += ["Scope: " + clean(path) for path in task.get("allowed_paths") or []]
        from panel_view import wrapped
        text = "\n".join(part for line in lines for part in wrapped(line, width, 8))
        navigation["detail"] = {"title": "REPO TASK / snapshot", "text": text}
        navigation["offset"] = 0


def read_evidence(repo, target):
    """A background evidence read that always names its target, even when it fails."""
    from terminal_panel import call_results, clean
    try:
        return target, call_results(repo, target), None
    except Exception as error:
        return target, {}, clean(str(error), 120) or "Result evidence unavailable"


def finish_opening(navigation, report):
    """Open the detail for a background evidence read that still matches what was asked to open."""
    target, evidence, error = report
    if navigation.get("opening") != target:
        return False
    navigation.pop("opening")
    if error:
        navigation["notice"] = error
    else:
        navigation.pop("notice", None)
        navigation["detail"] = {"title": "RECORDED RESULTS", "report": dict(
            evidence, rows=[r for r in evidence.get("rows", []) if r.get("calls")])}
        navigation["offset"] = 0
    return True


def expire_notice(navigation, now, limit=10):
    """Drop a notice that has been shown for `limit` seconds; True when it was removed."""
    text = navigation.get("notice")
    if not text:
        navigation.pop("notice_seen", None)
        return False
    seen = navigation.get("notice_seen")
    if not seen or seen[0] != text:
        navigation["notice_seen"] = (text, now)
        return False
    if now - seen[1] >= limit:
        navigation.pop("notice")
        navigation.pop("notice_seen", None)
        return True
    return False


def window_zoomed():
    """Whether this pane's window is zoomed; None when tmux cannot say."""
    from terminal_panel import tmux_command
    try:
        result = subprocess.run(tmux_command("display-message", "-p", "-t", os.environ.get("TMUX_PANE", ""),
                                             "#{window_zoomed_flag}"),
                                capture_output=True, text=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    value = result.stdout.strip() if result.returncode == 0 else ""
    return value == "1" if value in ("0", "1") else None


def call_results(repo, action):
    from terminal_panel import results
    kind, ident = action
    return (results(repo, task_id=ident[0], room_participant=ident[1]) if kind == "debate"
            else results(repo, room_participant=ident))


def resolve_call_alias(repo, room_id, value):
    from terminal_panel import events
    if not room_id or not re.fullmatch(r"[A-Za-z0-9-]{1,8}", value or ""):
        return value
    matches = {row.get("task_id") for row in json.loads(events(repo, "list", "--json", output=True))
               if row.get("task_id") and row.get("refs", {}).get("panel_alias") == value
               and room.same_room(repo, row.get("refs", {}).get("panel_room_id"), room_id)}
    if len(matches) > 1:
        raise ValueError("call alias resolves to more than one task in this room lineage")
    return next(iter(matches)) if matches else value


def render_call_view(repo, participant, view, width, task_id=None, cache=None):
    """Render one participant's retained result, event tail, or verified patch."""
    from terminal_panel import _review_verdict, results_snapshot, verified_patch
    from panel_view import call_identity
    from panel_results import _indexed

    cache = {} if cache is None else cache
    if "snapshot" not in cache:
        cache["snapshot"] = results_snapshot(repo, task_id, participant)
    report, (unused_attempts, indexed) = cache["snapshot"]
    attempts = unused_attempts
    if participant and not cache.get("filtered"):
        report["rows"] = [row for row in report["rows"] if row.get("calls")]
        cache["filtered"] = True
    row = next(iter(report.get("rows", [])), {})
    call = next((item for item in reversed(row.get("calls", [])) if item.get("attempt_id")), {})
    if participant and not call:
        # Artifactless failures have no indexed call row, but the same snapshot
        # carries the retained lifecycle projection and its workspace refs.
        attempt = next((item for item in reversed(attempts)
                        if item.get("refs", {}).get("panel_room_participant") == participant and
                        (task_id is None or item.get("task_id") == task_id)), {})
        if attempt:
            refs = attempt.get("refs", {})
            reason = attempt.get("reason_code")
            state = attempt.get("state")
            ending = ("guard_stop" if reason == "worker_authority_violation" else
                      ("verify_timed_out" if attempt.get("verify_exit") == 124 else "verify_failed")
                      if reason in {"verification_failed", "verify_failed"} else
                      "provider_error" if refs.get("provider_error_exit") else state)
            location = refs.get("panel_location")
            row = {"task_id": attempt.get("task_id"), "title": refs.get("panel_label"),
                   "model": refs.get("panel_model"), "role": refs.get("panel_role"),
                   "process_state": state, "location": location,
                   "next_action": ("oms panel --salvage " + str(attempt.get("task_id"))
                                   if isinstance(location, str) and location.startswith("worktree:") else
                                   "inspect the failure and retry or finalize the task")}
            call = {"attempt_id": attempt.get("attempt_id"), "state": state, "ending": ending,
                    "error": reason, "worktree": location if isinstance(location, str) and
                    location.startswith("worktree:") else None}
            report["rows"] = [row]
    model = row.get("model") or call.get("served_model") or call.get("selected_model") or call.get("requested_model")
    heading = call_identity(row.get("role") or "worker", model,
                            row.get("title") or row.get("task_id"), alias=call.get("alias")) if participant else None
    def reader_text(value):
        return panel_reader.ReaderText(value, artifact=(call.get("attempt_id"), call.get("artifact")),
                                       updated_at=call.get("ts") or row.get("updated_at"))
    if view in ("Summary", "Raw", "Run info"):
        if call.get("kind") == "review":
            call = dict(call)
            verdict, conflict = _review_verdict(call.get("answer"), with_conflict=True)
            call["review_verdict"] = ("GATE: " + verdict if verdict else "not recorded") + (
                " (conflicting declarations)" if conflict else "")
        return reader_text(panel_reader.render_view(view, report, call, width=width, heading=heading))

    if view == "Diff":
        patch, patch_error = None, False
        if call.get("patch"):
            try:
                patch = verified_patch(repo, call)
            except (OSError, ValueError, TypeError):
                patch_error = True
        return reader_text(panel_reader.render_view(view, report, call, patch=patch, width=width,
                                                    patch_error=patch_error, heading=heading))

    if view == "Events" and "timeline" not in cache:
        cache["timeline"] = (panel_reader.event_tail(repo, call.get("attempt_id"))
                             if call.get("attempt_id") else ([], False))
    timeline_rows, timeline_truncated = cache.get("timeline", ([], False)) if view == "Events" else ([], False)
    timeline = list(timeline_rows)
    task_id, attempt_id = row.get("task_id"), call.get("attempt_id")
    call_by_event = {item.get("event_id"): item for item in row.get("calls", []) if item.get("event_id")}
    for item in indexed:
        kind = item.get("kind")
        if ((item.get("attempt_id") == attempt_id and kind in ("review", "patch-admit")) or
                (kind == "patch-land" and item.get("task_id") == task_id and
                 item.get("patch_sha256") == call.get("patch_sha256")) or
                (item.get("task_id") == task_id and kind in ("panel-delivery", "panel-delivery-pending"))):
            detail = dict(item)
            review = call_by_event.get(item.get("event_id")) if kind == "review" else None
            if review:
                verdict, conflict = _review_verdict(review.get("answer"), with_conflict=True)
                detail["verdict"] = ("GATE: " + verdict if verdict else "GATE not recorded") + (
                    " (conflicting declarations)" if conflict else "")
                detail["summary"] = review.get("answer") or "review summary unavailable"
            if kind in ("panel-delivery", "panel-delivery-pending"):
                try:
                    delivery = json.loads(_indexed(repo, item, 4096))
                    detail["status"] = delivery.get("receipt", {}).get("status")
                except (OSError, ValueError, TypeError, AttributeError):
                    detail["summary"] = "delivery receipt unavailable"
            timeline.append(detail)
    if call.get("kind") == "review" and not any(item.get("kind") == "review" and
                                                    item.get("event_id") == call.get("event_id") for item in timeline):
        verdict, conflict = _review_verdict(call.get("answer"), with_conflict=True)
        review = {"kind": "review", "ts": call.get("ts"),
                  "verdict": ("GATE: " + verdict if verdict else "GATE not recorded") + (
                      " (conflicting declarations)" if conflict else ""),
                  "summary": call.get("answer") or "review summary unavailable"}
        timeline.append(review)
    return reader_text(panel_reader.render_view(view, report, call, timeline, width=width,
                                               timeline_truncated=timeline_truncated,
                                               heading=heading))


def advisor_command(repo, room_id, member):
    """A fresh control-path dispatch for one main: inherited caller identity is removed, never reused."""
    from terminal_panel import ENTRY
    return ["env", "-u", "OMS_ROOM_PARTICIPANT", "-u", "OMS_PANEL_MAIN_ATTEMPT", "-u", "OMS_ATTEMPT_ID",
            "bash", str(ENTRY), "panel", "--repo", str(repo), "--room", room_id, "--dispatch", "advisor",
            "--owner", member["provider"], "--main", member["participant"],
            "--label", "Advice for " + member["participant"]]


def ask_advisor(repo, state, navigation):
    """Open a question prompt for the selected main; the board never calls a model itself."""
    from terminal_panel import advisor_command, managed_session, tmux_command
    selected = navigation.get("selected") or ("",)
    target = selected if selected[0] == "chat" else navigation.get("primary")
    room_id = navigation.get("room_id")
    member = next((m for m in state.get("room", {}).get("participants", []) if target and room_id
                   and m["participant"] == target[1] and m["joined"] and m["role"] == "main"), None)
    if member is None:
        raise ValueError("select a joined main before asking an advisor")
    command = advisor_command(repo, room_id, member)
    if not managed_session():
        return "Ask from a terminal: " + " ".join(shlex.quote(part) for part in command[7:])
    # A failed dispatch keeps the popup open with its error instead of closing at once.
    script = (" ".join(shlex.quote(str(part)) for part in command) +
              '; s=$?; [ "$s" = 0 ] || { printf "\\nAdvisor call failed (exit %s). Press Enter to close. " "$s"; read -r _; }')
    script = "sh -c " + shlex.quote(script)  # independent of the user's tmux default-shell
    popup = tmux_command("display-popup", "-E", "-w", "80%", "-h", "60%", "-d", str(repo),
                         "-t", os.environ.get("TMUX_PANE", ""), script)
    # The popup runs beside the watcher, which keeps drawing; it is reaped on later loops.
    navigation.setdefault("popups", []).append(subprocess.Popen(popup, stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    return "Advisor question opened for " + member["participant"]


def worker_worktree(repo, member):
    """A finished worker's kept worktree: its recorded label must name exactly one git worktree."""
    from panel_view import TERMINAL_STATES
    label = member.get("location") or ""
    if member.get("role") == "main" or not label.startswith("worktree:") or label.count("/") != 1:
        raise ValueError("this call ran in the repository or recorded no worktree")
    if member.get("state") not in TERMINAL_STATES:
        raise ValueError("the worker is still running; its worktree belongs to it until it finishes")
    parent, name = label[len("worktree:"):].split("/")
    listed = subprocess.run(["git", "-C", str(repo), "worktree", "list", "--porcelain"], capture_output=True,
                            text=True, check=True, timeout=5, stdin=subprocess.DEVNULL).stdout
    paths = [Path(line[len("worktree "):].rstrip("\r")) for line in listed.splitlines() if line.startswith("worktree ")]
    found = [path for path in paths if path.name == name and path.parent.name == parent and path.is_dir()
             and path.resolve() != Path(repo).resolve()]
    if len(found) != 1:
        raise ValueError("worker worktree was cleaned up or is ambiguous; its recorded results remain")
    return found[0].resolve()


def open_worktree(repo, state, navigation):
    """Open a plain shell at a finished worker's worktree; it carries no worker authority."""
    from terminal_panel import managed_session, tmux_command, worker_worktree
    from room_view import nodes
    selected = navigation.get("selected") or ("",)
    member = next((m for m in nodes(state) if selected[0] == "result" and m["participant"] == selected[1]), None)
    if member is None:
        raise ValueError("select a worker before opening its worktree")
    path = worker_worktree(repo, member)
    if not managed_session():
        return "Worktree: " + str(path)
    subprocess.run(tmux_command("new-window", "-n", "wt-" + member["participant"][-8:], "-c", str(path)),
                   check=True, timeout=3, stdin=subprocess.DEVNULL)
    return "Worktree shell opened; edits there are not part of the worker's recorded patch"


def tab_event(event, navigation):
    """The bottom box's own keys and clicks; True when the event was consumed. Tabs never switch by themselves."""
    from room_view import TABS
    names = navigation.get("visible_tabs", [key for key, unused in TABS if key != "needs"])
    tab = navigation.get("tab") if navigation.get("tab") in names else "log"
    kind = event[0]
    if navigation.get("surface") not in {"graph", "tree"} or navigation.get("detail"):
        return False
    log_box = navigation.get("log_box") or {}
    log_y = event[3] if kind == "scroll" and len(event) > 3 else None
    if kind == "scroll" and log_y in range(log_box.get("y1", -1), log_box.get("y2", -2) + 1):
        navigation["log_offset"] = max(0, navigation.get("log_offset", 0) - event[1])
        return True
    if tab == "log" and kind in {"[", "]"}:
        navigation["log_offset"] = max(0, navigation.get("log_offset", 0) + (3 if kind == "[" else -3))
        return True
    if kind == "tab":
        if not navigation.get("tab_strip_visible", True):
            return False
        navigation["tab"] = names[(names.index(tab) + 1) % len(names)]
        return True
    hit = next((h for h in navigation.get("hits", []) if kind == "click" and h["y"] == event[2]
                and h["x1"] <= event[1] <= h["x2"] and h["action"][0] in {"tab", "message", "seat"}), None)
    if hit and hit["action"][0] == "tab":
        navigation["tab"] = hit["action"][1]
        return True
    box = navigation.get("box") or {}
    inside = bool(box) and (event[3] if kind == "scroll" and len(event) > 3 else box["y1"]) in range(box["y1"], box["y2"] + 1)
    if tab == "messages" and kind != "q" and (hit is not None or kind != "click") and inside:
        return panel_messages.handle(event, navigation)
    if tab == "debate" and kind != "q" and (hit is not None or kind != "click") and inside:
        return panel_debate.handle(event, navigation)
    if tab == "plan" and kind != "q" and inside:
        from room_view import plan_event
        if plan_event(event, navigation):
            return True
    if tab in {"past", "needs"} and kind != "q" and inside:
        from room_view import past_event
        return past_event(event, navigation, tab)
    return False


BOTTOM_OPTION = "@oms_panel_bottom"
BOTTOM_LIMIT = 512
BOTTOM_KINDS = {"chat", "result", "debate", "mail"}
HEADER_OPTION = "@oms_panel_header"
HEADER_ENABLED = "@oms_panel_headers"


def _header_safe(value):
    from terminal_panel import clean
    return (clean(value, 120) or "").replace("#", "##").replace(",", " ").replace("{", "(").replace("}", ")")


def context_header_text(state, main_attempt, repo):
    from terminal_panel import _header_safe
    from room_view import nodes, model_name, context_note
    from panel_view import call_identity, status_tag
    member = next((m for m in nodes(state) if m.get("role") == "main" and
                   main_attempt in (m.get("participant"), m.get("attempt"))), None)
    task = _header_safe(member.get("task_id")) if member and member.get("task_id") else "task unrecorded"
    model = _header_safe(model_name(member)) if member else "unknown"
    provider = _header_safe({"claude": "Claude Code", "codex": "Codex", "pi": "Pi"}.get(
        member.get("harness") or member.get("provider"), member.get("provider"))) if member else "unknown"
    status = _header_safe(status_tag(member.get("state") or "unknown")) if member else _header_safe(status_tag("unknown"))
    context = ""
    if member:
        note = context_note(state, member)
        if note:
            context = "   ctx %s%% used" % note[0]
    branch = _header_safe((state.get("repo") or {}).get("branch"))
    location = _header_safe((state.get("repo") or {}).get("name") or repo.name)
    if branch:
        location += "/" + branch
    return "%s   %s%s   via %s · %s" % (_header_safe(call_identity("main", model, task)),
                                        status, context, provider, location)


def _tmux_output_value(value):
    return value[:-1] if value.endswith("\n") else value


def _window_option_local(pane, option):
    from terminal_panel import _tmux_output_value, tmux_command
    present = subprocess.run(tmux_command("show-options", "-w", "-q", "-t", pane, option),
                             capture_output=True, text=True, check=False, timeout=2,
                             stdin=subprocess.DEVNULL)
    if present.returncode:
        return None, ""
    exists = bool(present.stdout.rstrip("\r\n"))
    if not exists:
        return False, ""
    raw = subprocess.run(tmux_command("show-options", "-w", "-v", "-q", "-t", pane, option),
                         capture_output=True, text=True, check=False, timeout=2,
                         stdin=subprocess.DEVNULL)
    if raw.returncode:
        return None, ""
    return True, _tmux_output_value(raw.stdout)


def update_context_header(state, main_attempt, repo, navigation=None):
    from terminal_panel import HEADER_ENABLED, HEADER_OPTION, _tmux_output_value, _window_option_local
    from terminal_panel import context_header_text, managed_session
    pane = os.environ.get("TMUX_PANE", "")
    if not managed_session() or not re.fullmatch(r"%[0-9]+", pane):
        return
    quiet = dict(capture_output=True, text=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
    def run(*args):
        from terminal_panel import tmux_command
        return subprocess.run(tmux_command(*args), **quiet)
    try:
        enabled = run("show-option", "-w", "-v", "-t", pane, HEADER_ENABLED)
        if enabled.returncode:
            originals = []
            for option, saved in (("pane-border-status", "@oms_panel_headers_previous_status"),
                                  ("pane-border-format", "@oms_panel_headers_previous_format")):
                local, value = _window_option_local(pane, option)
                if local is None:
                    return
                originals.append((option, saved, local, value))
            for option, saved, local, value in originals:
                run("set-option", "-w", "-t", pane, saved + "_local", "1" if local else "0")
                run("set-option", "-w", "-t", pane, saved, value)
            run("set-option", "-w", "-t", pane, HEADER_ENABLED, "1")
            run("set-option", "-w", "-t", pane, "pane-border-status", "top")
            run("set-option", "-w", "-t", pane, "pane-border-format", "#{?@oms_panel_headers,#{@oms_panel_header},}")
        label = context_header_text(state, main_attempt, repo)
        native = run("show-option", "-w", "-v", "-t", pane, "@oms_panel_native_pane")
        target = native.stdout.strip() if native.returncode == 0 else ""
        if re.fullmatch(r"%[0-9]+", target):
            width = run("display-message", "-p", "-t", target, "#{pane_width}")
            if width.returncode == 0 and width.stdout.strip().isdigit():
                from dashboard_projection import fit
                label = fit(label, int(width.stdout.strip()))
            current = run("show-option", "-p", "-v", "-t", target, HEADER_OPTION)
            if current.returncode or _tmux_output_value(current.stdout) != label:
                run("set-option", "-p", "-t", target, HEADER_OPTION, label)
        reader = panel_reader.find(pane)
        if reader:
            # The reader shell pins its identity; an additional pane title would repeat it.
            label = ""
            current = run("show-option", "-p", "-v", "-t", reader, HEADER_OPTION)
            if current.returncode or _tmux_output_value(current.stdout) != label:
                run("set-option", "-p", "-t", reader, HEADER_OPTION, label)
    except Exception:
        return


def toggle_context_headers():
    from terminal_panel import HEADER_ENABLED, _tmux_output_value, managed_session
    pane = os.environ.get("TMUX_PANE", "")
    if not managed_session() or not re.fullmatch(r"%[0-9]+", pane):
        return
    def run(*args):
        from terminal_panel import tmux_command
        return subprocess.run(tmux_command(*args), capture_output=True, text=True, check=False,
                              timeout=2, stdin=subprocess.DEVNULL)
    try:
        enabled = run("show-option", "-w", "-v", "-t", pane, HEADER_ENABLED)
        if enabled.returncode:
            return
        if enabled.returncode == 0 and enabled.stdout.strip() == "0":
            run("set-option", "-w", "-t", pane, HEADER_ENABLED, "1")
            run("set-option", "-w", "-t", pane, "pane-border-status", "top")
            run("set-option", "-w", "-t", pane, "pane-border-format", "#{?@oms_panel_headers,#{@oms_panel_header},}")
        else:
            run("set-option", "-w", "-t", pane, HEADER_ENABLED, "0")
            for option, saved in (("pane-border-status", "@oms_panel_headers_previous_status"),
                                  ("pane-border-format", "@oms_panel_headers_previous_format")):
                local = run("show-option", "-w", "-v", "-t", pane, saved + "_local")
                if local.returncode:
                    continue
                if local.stdout.strip() == "1":
                    value = run("show-option", "-w", "-v", "-t", pane, saved)
                    if value.returncode == 0:
                        run("set-option", "-w", "-t", pane, option, _tmux_output_value(value.stdout))
                elif local.stdout.strip() == "0":
                    run("set-option", "-w", "-u", "-t", pane, option)
    except Exception:
        return


def bottom_state(navigation):
    """The bottom box as this board shows it: tab, previewed target and detail scroll."""
    from terminal_panel import BOTTOM_KINDS
    from room_view import TABS
    tab = navigation.get("tab") if navigation.get("tab") in dict(TABS) else "log"
    target = (navigation.get("preview") or {}).get("target")
    if not (isinstance(target, tuple) and len(target) == 2 and target[0] in BOTTOM_KINDS):
        target = None
    offset = (navigation.get("band_offsets") or {}).get("detail", 0)
    return {"tab": tab, "target": target, "offset": offset if isinstance(offset, int) and offset > 0 else 0,
            "dismissed": bool(navigation.get("dismissed"))}


def bottom_text(state):
    def listed(value):
        return [listed(item) for item in value] if isinstance(value, tuple) else value
    return json.dumps(dict(state, target=listed(state["target"])), separators=(",", ":"))


def read_bottom(raw):
    """A shared bottom state, or None when the option is unset, oversized or malformed."""
    from terminal_panel import BOTTOM_KINDS, BOTTOM_LIMIT
    from room_view import TABS
    if not raw or len(raw.encode("utf-8", "replace")) > BOTTOM_LIMIT:
        return None
    try:
        value = json.loads(raw)
    except ValueError:
        return None
    if (not isinstance(value, dict) or set(value) - {"tab", "target", "offset", "dismissed"}
            or value.get("tab") not in dict(TABS) or not isinstance(value.get("dismissed", False), bool)):
        return None
    ident = lambda item: isinstance(item, str) and re.fullmatch(r"[A-Za-z0-9._:/-]{1,128}", item) is not None
    target, offset = value.get("target"), value.get("offset", 0)
    if target is not None:
        if not (isinstance(target, list) and len(target) == 2 and target[0] in BOTTOM_KINDS):
            return None
        if target[0] == "debate":
            if not (isinstance(target[1], list) and len(target[1]) == 2 and all(ident(i) for i in target[1])):
                return None
            target = (target[0], tuple(target[1]))
        elif ident(target[1]):
            target = tuple(target)
        else:
            return None
    if not isinstance(offset, int) or isinstance(offset, bool) or not 0 <= offset <= 100000:
        return None
    return {"tab": value["tab"], "target": target, "offset": offset, "dismissed": value.get("dismissed", False)}


def apply_bottom(navigation, state, shared):
    """Show the panel session's bottom box; a target naming nobody in this room is left alone."""
    navigation["tab"] = shared["tab"]
    navigation.setdefault("band_offsets", {})["detail"] = shared["offset"]
    # Esc in one window empties the box in every window, instead of each showing its own default.
    if shared["dismissed"]:
        navigation["dismissed"] = True
    else:
        navigation.pop("dismissed", None)
    target = shared["target"]
    if target is None:
        navigation.pop("preview", None)
        return
    members = {m.get("participant") for m in state.get("room", {}).get("participants") or [] if isinstance(m, dict)}
    named = target[1][1:] if target[0] == "debate" else (target[1],)
    if all(who in members for who in named):
        if (navigation.get("preview") or {}).get("target") != target:
            navigation["preview"] = {"target": target}


def publish_bottom(navigation, before):
    """Write the session's bottom box when one input event changed it; render clamps and unchanged frames write nothing."""
    from terminal_panel import BOTTOM_OPTION, bottom_state, bottom_text, managed_session, tmux_command
    state = bottom_state(navigation)
    if before is None or state == before:
        return
    navigation["bottom_seen"] = state
    if managed_session():
        subprocess.run(tmux_command("set-option", "-t", "=" + os.environ.get("OMS_PANEL_SESSION", "") + ":",
                                    BOTTOM_OPTION, bottom_text(state)),
                       check=False, timeout=2, stdin=subprocess.DEVNULL)


def refreshed_navigation(navigation, state, bottom=None, close_call=None):
    """Apply a new snapshot: another room resets the view; an open preview re-reads its evidence."""
    from terminal_panel import apply_bottom, read_bottom
    ident = state.get("room", {}).get("id")
    if navigation.get("room_id") and ident and navigation["room_id"] != ident:
        if close_call:
            close_call()
        navigation = {"collapsed": set(), "offset": 0}
    elif navigation.get("preview"):
        # Evidence is re-read only when the room's messages or a call's state moved, not on every refresh.
        attempts = state.get("attempts") or {}
        signature = (state.get("room", {}).get("message_count"), tuple(
            (row.get("attempt_id"), row.get("state")) for key in ("active_recent", "recent")
            for row in attempts.get(key) or [] if isinstance(row, dict)))
        if signature != navigation.get("preview_signature"):
            navigation["preview"]["refresh"] = True
        navigation["preview_signature"] = signature
    if (navigation.get("debate_view") or {}).get("shown"):
        navigation["debate_view"]["refresh"] = True
    # The bottom box is the session's: a tab, target or scroll chosen in one window reaches every board in one refresh.
    shared = read_bottom(bottom)
    if shared and shared != navigation.get("bottom_seen"):
        navigation["bottom_seen"] = shared
        apply_bottom(navigation, state, shared)
    return navigation
