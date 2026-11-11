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


WAKE_IDLE_SECONDS = 1800
WAKE_DAILY_LIMIT = 6
_CHILD_ROLES = ("worker", "advisor", "reviewer", "researcher")


def _one_line(text):
    return " ".join("".join(" " if unicodedata.category(c).startswith("C") else c for c in str(text)).split())


def help_rows(rows, width, unicode=True):
    """Expose the room prerequisite in expanded help."""
    from panel_view import box_row
    return rows[:-1] + [box_row("ROOM".ljust(9) + "[o] Rooms before main/task shortcuts in roomless control",
                                width, unicode)] + rows[-1:]


def wake_reason(repo):
    """Why an idle main should continue, or "" when no shared work waits: ready plan tasks, a proposal under review."""
    from terminal_panel import ENTRY, _read
    reasons = []
    listed = subprocess.run(["bash", str(ENTRY), "agent-plan", "--repo", str(repo), "ready"], capture_output=True,
                            text=True, check=False, timeout=60, stdin=subprocess.DEVNULL)
    # Sorted, so the same waiting work always yields the same dedupe key.
    ready = sorted({_one_line(line)[:80] for line in listed.stdout.splitlines() if line.strip()}) if listed.returncode == 0 else []
    if ready:
        reasons.append("ready plan tasks " + ", ".join(ready[:8]) + (" (+%d more)" % (len(ready) - 8) if len(ready) > 8 else ""))
    raw = _read(repo, ".oms/plan/autopilot-run.json", 1024 * 1024, missing=True, label="autopilot receipt")
    try:
        receipt = json.loads(raw) if raw else {}
    except ValueError:
        receipt = {}
    if isinstance(receipt, dict) and receipt.get("stage") == "proposal-review":
        reasons.append("an autopilot proposal waits in proposal-review")
    return "; ".join(reasons)


def wake_prompt(reason):
    """One provider-neutral line; send-keys types it literally, so it must hold no newline."""
    return _one_line("OMS tick wake: %s. Continue the shared plan (oms agent-plan list) within your existing "
                     "authority; if the next step needs the user's decision, authority you do not have, or a hard "
                     "blocker, say which and stop." % reason)


def _wake_target(repo, now):
    """(pane, participant) of the one verified panel main when it has no live child and has idled long enough."""
    from panel_native import _swap_main_window, live_main_pane
    from terminal_panel import _read, main_history, resolve_session
    pane, unused = live_main_pane(repo)
    session, unused = resolve_session(repo, wait=False)
    unused, attempt, main_pane, member, ident = _swap_main_window(repo, session)
    if main_pane != pane:
        raise ValueError("main pane changed during verification")
    if any(row.get("terminal") is False and (row.get("refs") or {}).get("panel_role") in _CHILD_ROLES
           for row in main_history(repo, ident)):
        raise ValueError("main has a live child call")
    raw = _read(repo, ".oms/hooks/panel-activity/" + member + ".json", 4096, missing=True, label="panel activity")
    try:
        row = json.loads(raw) if raw else {}
    except ValueError:
        row = {}
    since = row.get("since") if isinstance(row, dict) else None
    # No idle record (a provider without the activity hook) is not proof of idleness.
    if (not isinstance(row, dict) or row.get("state") != "idle" or row.get("attempt") != attempt
            or type(since) not in (int, float) or not 0 <= since <= now or now - since < WAKE_IDLE_SECONDS):
        raise ValueError("main is not idle for %d minutes" % (WAKE_IDLE_SECONDS // 60))
    return pane, member


class LedgerRefused(ValueError):
    """The tick receipt holding the wake ledger cannot be trusted; it is left as found."""


TICK_RECEIPT_MAX = 1024 * 1024


def read_tick_receipt(path):
    """(receipt, wakes) of a tick receipt; the one check both the wake and `oms tick run` apply before writing it.

    Only a missing file is empty history. An empty, symlinked, hard-linked, non-regular, oversized or malformed
    receipt raises LedgerRefused, and its file must be kept as found so the wake's cap and dedupe never reset."""
    import stat
    try:
        named = os.lstat(path)
    except FileNotFoundError:
        return {}, []
    except OSError as error:
        raise LedgerRefused("tick receipt cannot be inspected") from error
    if not stat.S_ISREG(named.st_mode) or named.st_nlink != 1:
        raise LedgerRefused("tick receipt is a symlink, a hard link or not a regular file")
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(fd, "rb") as handle:
            opened = os.fstat(handle.fileno())
            raw = handle.read(TICK_RECEIPT_MAX + 1)
    except OSError as error:
        raise LedgerRefused("tick receipt is unreadable") from error
    if ((opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino) or opened.st_nlink != 1
            or len(raw) > TICK_RECEIPT_MAX):
        raise LedgerRefused("tick receipt changed while read or is too large")
    try:
        receipt = json.loads(raw.decode("utf-8"))
    except (ValueError, RecursionError) as error:
        raise LedgerRefused("tick receipt is empty or not JSON") from error
    wakes = receipt.get("wakes", []) if isinstance(receipt, dict) else None
    if not isinstance(wakes, list) or not all(
            isinstance(row, dict) and isinstance(row.get("day"), str) and isinstance(row.get("main"), str)
            for row in wakes):
        raise LedgerRefused("tick receipt or its wakes ledger is malformed")
    return receipt, wakes


def _wake_ledger(repo, day):
    """(receipt, today's wake rows) from the receipt `oms tick run` also writes."""
    receipt, wakes = read_tick_receipt(str(Path(repo) / ".oms" / "tick" / "last.json"))
    return receipt, [row for row in wakes if row["day"] == day]


def wake_summary(wakes, day):
    """One line for `oms tick status`: how much of today's wake allowance the tick spent, and on what.

    Counts come from the ledger the wake itself writes; a skipped wake spends nothing. The ledger records
    no token count, cost or wall time for a wake, so the line says so instead of printing a zero."""
    today = [row for row in wakes if isinstance(row, dict) and row.get("day") == day]
    spent = [row for row in today if row.get("outcome") != "skipped"]
    resumed = sum(row.get("outcome") == "resumed" for row in spent)
    parts = []
    if len(spent) - resumed:
        parts.append("%d main prompt(s)" % (len(spent) - resumed))
    if resumed:
        parts.append("%d autopilot resume(s)" % resumed)
    skipped = len(today) - len(spent)
    return "wakes today (UTC): %d of %d spent%s%s; tokens, cost and wall time: not recorded" % (
        len(spent), WAKE_DAILY_LIMIT, " (%s)" % ", ".join(parts) if parts else "",
        "; %d skipped" % skipped if skipped else "")


def wake_summary_cli(argv):
    """`oms tick status` helper: RECEIPT -> the summary line, or nothing when no wake was ever recorded."""
    try:
        wakes = read_tick_receipt(argv[0])[1]
    except LedgerRefused as error:
        print("wakes today (UTC): unknown (%s)" % error)
        return 0
    if wakes:
        print(wake_summary(wakes, time.strftime("%Y-%m-%d", time.gmtime())))
    return 0


def _record_wake(repo, receipt, wakes):
    import tempfile
    directory = Path(repo) / ".oms" / "tick"
    directory.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(dir=str(directory), prefix=".wake.")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            json.dump(dict(receipt, wakes=wakes), output, ensure_ascii=False, sort_keys=True)
            output.write("\n")
        os.replace(temporary, str(directory / "last.json"))
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise


RESUME_SUBJECT = "autopilot"
_RESUME_AFTER = {
    "proposal-review": "the parent reviews the proposal and runs the printed `oms autopilot run --proposal FILE --expected-proposal-sha256 SHA`",
    "parked": "the operator resolves the park (`oms autopilot --repo . status`) or abandons it",
    "done": "a new `oms autopilot propose` binds the next contract",
}


def _live_main_blocker(repo):
    """"" only when no panel main is live; otherwise why one may be. Unverified liveness counts as live.

    The start path's evidence, read-only: an open panel-main attempt blocks unless its host and native process are
    proven gone (the proof start uses to release it), and a session window for this checkout blocks while it holds a main."""
    import panel_release
    from panel_native import live_main_refusal, live_mains
    from terminal_panel import events, recorded_room, resolve_session
    try:
        history = json.loads(events(repo, "list", "--json", output=True))
        if not isinstance(history, list) or any(not isinstance(row, dict) for row in history):
            raise ValueError("main lifecycle status unavailable")
        dead = {item["attempt"] for item in panel_release.classify(history)["releasable"]}
        if any(row.get("tool") == "panel-main" and row.get("terminal") is not True and row.get("attempt_id") not in dead
               for row in history):
            return _one_line(live_main_refusal(repo, history))[:200]
        if shutil.which("tmux"):
            session, owner = resolve_session(repo, wait=False)
            if owner == str(repo):
                # Releasable dead attempts (released by the panel, never here) must not make a control-only session
                # look occupied.
                if live_mains(repo, session, recorded_room(session, checked=True), released=dead) > 0:
                    return "a panel main window is live"
        return ""
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        return "main liveness unverified: " + _one_line(error)[:160]


def _spawn_continuation(argv, log, cwd):
    """Start the approved continuation detached (own session, no inherited descriptors); returns its pid, never waits."""
    fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_APPEND | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        # The marker keeps an unattended resume from refreshing the approval time the age bound reads.
        return subprocess.Popen(argv, cwd=cwd, stdin=subprocess.DEVNULL, stdout=fd, stderr=subprocess.STDOUT,
                                close_fds=True, start_new_session=True,
                                env=dict(os.environ, OMS_AUTOPILOT_WAKE_RESUME="1")).pid
    finally:
        os.close(fd)


APPROVAL_MAX_AGE_DAYS = 7


def _stale_approval(rules, receipt_path, row, now):
    """"" when the approval may be resumed, else why not. OMS_TICK_AUTOPILOT_MAX_AGE_DAYS bounds the age
    (default 7, 0 disables); an approval with no recorded time is not refused (its clock starts at the
    first headless resume), an unreadable approval ledger is."""
    raw = os.environ.get("OMS_TICK_AUTOPILOT_MAX_AGE_DAYS", "").strip()
    limit = int(raw) if raw.isdigit() else APPROVAL_MAX_AGE_DAYS
    if limit <= 0:
        return ""
    try:
        recorded = rules["approval_time"](receipt_path, row)
    except rules["ReceiptError"] as error:
        return "%s; the approval age cannot be judged" % error
    if recorded is None or now - recorded[0] <= limit * 86400:
        return ""
    return "autopilot approval is %dd old (approved %s by %s; limit %dd)" % (
        (now - recorded[0]) // 86400, time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(recorded[0])),
        "a person's run" if recorded[1] == "run" else "its first headless resume", limit)


def resume_autopilot(repo, dry_run=False, now=None, once=None):
    """No live panel main: run the receipt's own `autopilot run` continuation headlessly, once; returns a status line.

    Only a contract the user already approved resumes: the receipt must load, pass the digest gate every resume passes
    (reenter_judgment: not done, proposal-review or parked, no living claimant) and print a run continuation carrying
    its approved proposal digest. Nothing is created, applied or overridden. The launch is recorded in the wake ledger
    (one pool of WAKE_DAILY_LIMIT per UTC day) before the spawn; ONCE is claimed so a tick run starts at most one."""
    import hashlib
    import runpy
    import shlex
    from terminal_panel import _lock
    repo = Path(repo)
    now = time.time() if now is None else now
    day = time.strftime("%Y-%m-%d", time.gmtime(now))
    receipt_path = repo / ".oms" / "plan" / "autopilot-run.json"
    state_repo = os.path.realpath(repo)

    def skip(why, until):
        return "skipped: %s; resumes when %s" % (why, until)

    def attempt():
        ledger, wakes = _wake_ledger(repo, day)
        blocker = _live_main_blocker(repo)
        if blocker:
            return skip("a panel main may be live (%s)" % blocker, "it exits or its termination is proven")
        if not os.path.lexists(receipt_path):
            return skip("no live main and no autopilot contract", "`oms autopilot propose` and a reviewed `run --proposal` bind one")
        rules = runpy.run_path(str(Path(__file__).with_name("autopilot-receipt.py")))
        status = "`oms autopilot --repo %s status` shows a valid receipt" % repo
        try:
            rules["validate_receipt_location"](receipt_path, state_repo)
            row, payload = rules["load_receipt"](receipt_path)
            # The caller is the checkout this tick runs for: a receipt bound to any other checkout (a scratch
            # worktree) is refused by validate_live, as it is for `autopilot run` from the state root.
            refusal = rules["reenter_judgment"](receipt_path, row, state_repo, 0, 0, True, state_repo)[0]
        except (OSError, ValueError, KeyError, TypeError, rules["ReceiptError"]) as error:
            return skip("autopilot receipt invalid (%s)" % _one_line(error)[:120], status + " or it is abandoned and re-proposed")
        stage, digest = row["stage"], rules["digest"](payload)
        if refusal:
            diagnostic = row.get("diagnostic") if stage == "parked" else None
            reason = refusal + ("; next step: %s (%s)" % (_one_line(diagnostic.get("reason")), _one_line(diagnostic.get("reason_code")))
                                if isinstance(diagnostic, dict) else "")
            return skip("autopilot %s: %s" % (stage, reason),
                        _RESUME_AFTER.get(stage, status + " and no live session holds the run"))
        argv, proposal = rules["option_args"](row), row["proposal"]
        if (argv[:2] != ["oms", "autopilot"]
                or argv[-5:] != ["run", "--proposal", proposal["path"], "--expected-proposal-sha256", proposal["sha256"]]):
            return skip("autopilot continuation at stage %s is not an approved `run`" % stage,
                        "the parent approves a proposal with `oms autopilot run --proposal FILE --expected-proposal-sha256 SHA`")
        spent = [w for w in wakes if w.get("outcome") != "skipped"]
        key = hashlib.sha256("\0".join((RESUME_SUBJECT, day, digest)).encode()).hexdigest()[:16]
        stale = _stale_approval(rules, receipt_path, row, now)
        if stale:
            # A refusal to start, recorded once per receipt state and day; running work is never touched.
            if not dry_run and not any(w["main"] == RESUME_SUBJECT and w.get("key") == key for w in wakes):
                _record_wake(repo, ledger, wakes + [{
                    "day": day, "main": RESUME_SUBJECT, "key": key, "outcome": "skipped", "reason": stale,
                    "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now)), "stage": stage, "receipt": digest}])
            return skip(stale, "a person re-approves it (`oms autopilot --repo %s reenter`), or "
                               "OMS_TICK_AUTOPILOT_MAX_AGE_DAYS is raised (0 disables)" % repo)
        if any(w["main"] == RESUME_SUBJECT and w.get("key") == key for w in spent):
            return skip("autopilot already resumed today at this receipt state", "the receipt changes or the UTC day rolls over")
        if len(spent) >= WAKE_DAILY_LIMIT:
            return skip("%d wakes already spent today" % len(spent), "the UTC day rolls over")
        command = ["bash", str(Path(__file__).resolve().parents[1] / "autopilot.sh")] + argv[2:]
        shown = " ".join(shlex.quote(part) for part in argv)
        if dry_run:
            return "would resume autopilot (stage %s, receipt %s): %s" % (stage, digest[:12], shown)
        try:
            if once:
                os.close(os.open(once, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
        except FileExistsError:
            return skip("this tick run already resumed one autopilot", "the next tick run")
        except OSError as error:
            return skip("cannot claim this tick run's resume marker (%s)" % _one_line(error)[:80], "the marker is writable")
        log = repo / ".oms" / "tick" / ("autopilot-%s.log" % digest[:12])
        _record_wake(repo, ledger, wakes + [{"day": day, "main": RESUME_SUBJECT, "key": key, "outcome": "resumed",
                                             "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now)),
                                             "stage": stage, "receipt": digest}])
        pid = _spawn_continuation(command, str(log), state_repo)
        return "resumed autopilot (stage %s, receipt %s, pid %d, output %s): %s" % (
            stage, digest[:12], pid, log.relative_to(repo), shown)

    try:
        if dry_run:
            return attempt()
        # Main start takes this lock too, so no main can start between the liveness proof and the spawn.
        with _lock(repo, "panel-main-start", timeout=.1):
            return attempt()
    except LedgerRefused as error:
        return ("refused: %s; .oms/tick/last.json is kept as found: inspect it, then move it aside to reset "
                "today's wake ledger" % error)
    except ValueError as error:
        if "busy" in str(error):
            return "skipped: panel typing lock held"
        return "skipped: " + _one_line(error)[:200]


def wake_main(repo, dry_run=False, now=None, once=None):
    """Tick's opt-in wake: at most one prompt into an idle panel main while shared work waits; returns a status line.

    With no live main it resumes an approved autopilot contract instead (resume_autopilot; ONCE is its per-run marker).

    The wake is recorded before it is typed, so a retried or killed tick never submits it twice. A pane whose
    input line is not provably empty is skipped and the skip recorded; nothing is cleared, typed or entered."""
    import hashlib
    from panel_native import PaneChanged, input_line_empty, live_main_pane, pane_stamp, submit_owned
    from terminal_panel import _lock
    repo = Path(repo)
    now = time.time() if now is None else now
    day = time.strftime("%Y-%m-%d", time.gmtime(now))

    def attempt():
        receipt, wakes = _wake_ledger(repo, day)
        pane, member = _wake_target(repo, now)
        spent = [row for row in wakes if row.get("outcome") != "skipped"]
        key = hashlib.sha256("\0".join((member, day, reason)).encode()).hexdigest()[:16]
        if any(row["main"] == member and row.get("key") == key for row in spent):
            return "skipped: %s already woken today for this work" % member
        if len(spent) >= WAKE_DAILY_LIMIT:
            return "skipped: %s reached %d wakes today" % (member, WAKE_DAILY_LIMIT)
        prompt = wake_prompt(reason)
        stamp = pane_stamp(pane)
        empty, why = input_line_empty(pane)
        if dry_run:
            return ("would wake %s: %s" % (member, prompt)) if empty else "would skip %s: %s" % (member, why)
        row = {"day": day, "main": member, "key": key, "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now))}
        if not empty:
            _record_wake(repo, receipt, wakes + [dict(row, outcome="skipped", reason=why)])
            return "skipped: %s; %s" % (member, why)
        _record_wake(repo, receipt, wakes + [dict(row, outcome="woke")])
        try:
            submit_owned(pane, stamp, prompt)
        except (PaneChanged, subprocess.CalledProcessError) as error:
            # tmux refused, or the pane changed hands: nothing was delivered, so "woke" must not stand.
            why = str(error) if isinstance(error, PaneChanged) else "send-keys failed (exit %s)" % error.returncode
            _record_wake(repo, receipt, wakes + [dict(row, outcome="skipped", reason=why)])
            return "skipped: %s; %s" % (member, why)
        return "woke %s: %s" % (member, prompt)

    try:
        blocker = _live_main_blocker(repo)
        if not blocker:
            return resume_autopilot(repo, dry_run, now, once)
        try:
            live_main_pane(repo)
        except ValueError as error:
            # Neither a pane to type into nor proof the main is gone: say why, with the recovery command if it has one.
            return "skipped: no live main pane to type into (%s); %s" % (_one_line(error)[:100], blocker)
        reason = wake_reason(repo)
        if not reason:
            return "skipped: no ready plan work or pending proposal"
        if dry_run:
            return attempt()
        # swap_main verifies and types under this lock; a wake must not interleave with a swap or start.
        with _lock(repo, "panel-main-start", timeout=.1):
            return attempt()
    except LedgerRefused as error:
        return ("refused: %s; .oms/tick/last.json is kept as found: inspect it, then move it aside to reset "
                "today's wake ledger" % error)
    except ValueError as error:
        if "busy" in str(error):
            return "skipped: panel typing lock held"
        return "skipped: " + _one_line(error)[:200]


def wake_cli(argv):
    """`oms tick` entry: REPO [--dry-run] [--once MARKER]; prints one status line."""
    repo, rest, dry, once = argv[:1], argv[1:], False, None
    while rest and rest[0] in ("--dry-run", "--once"):
        if rest[0] == "--once":
            once, rest = (rest[1] if len(rest) > 1 else None), rest[2:]
        else:
            dry, rest = True, rest[1:]
    if len(repo) != 1 or rest or (argv.count("--once") and not once):
        print("usage: wake REPO [--dry-run] [--once MARKER]", file=sys.stderr)
        return 2
    try:
        print(wake_main(repo[0], dry_run=dry, once=once))
    except (OSError, subprocess.SubprocessError) as error:
        print("skipped: " + _one_line(error)[:200])
    return 0


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
    from terminal_panel import clean, finish_opening, managed_session, menu_input, navigate, read_evidence, read_field, reader_shell, tab_event
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
                    if navigation.get("surface") == "inbox" and navigation.get("selected_row") and "state" in cache:
                        import panel_attention_read
                        read = panel_attention_read.mark_open(repo, os.environ.get("OMS_ROOM_ID", ""), cache["state"],
                                                              navigation["selected_row"][0])
                        if read is not None:
                            cache["state"] = dict(cache["state"], attention_read=read)
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
    from terminal_panel import (ENTRY, MAIN_HARNESSES, MAX_PROMPT, NativeShutdown, SESSION_ID, _adopt_successor,
        _LAUNCH_HARNESSES, caller_owner, clean, clear_control_view, council, dispatch, ensure_room,
        finalize_selection, hold_state, live_main_refusal, main_installed, main_missing, main_occupied,
        main_owner, managed_session, menu_rows, native_adapter, native_binary, open_native,
        panel_mains, panel_room, private_capacity, provider_catalog, raw_keys, read_choice, read_field, recorded_room,
        reopen_panel, retry_delivery, room_listing, room_open, run, safe_label, show_providers, snapshot,
        source_stamps, stale_notice, swap_choices, swap_main, terminal_style, tmux_command)
    harness = os.environ.get("OMS_PANEL_HARNESS") or next(iter(panel_mains()), None)
    provider = os.environ.get("OMS_PANEL_OWNER") or main_owner(harness) or caller_owner()
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
                if adopted:
                    navigation.pop("room_poll_unknown", None)
                else:
                    navigation["room_poll_unknown"] = "unset"
                    previous = "No room selected; use o Rooms to select or create one, or p Providers > Native"
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
                if navigation.get("room_poll_unknown") == "unset":
                    raise ValueError("No room selected; use o Rooms to select or create one, or p Providers > Native")
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
                selected = {"1": "claude", "2": "pi", "0": "codex"}.get(choice) or read_field("Harness (claude/pi/codex/agy or opted-in native adapter): ")
                if selected not in _LAUNCH_HARNESSES:
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
                selected = read_field("Main harness (claude/pi/codex/agy): ")
                if selected not in _LAUNCH_HARNESSES:
                    raise ValueError("select claude, pi, codex or agy")
                harness, provider = selected, main_owner(selected)
                previous = "main provider selected"
            elif choice == "t":
                if not harness:
                    raise ValueError("Select a main harness first (4)")
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
                        if selected == "antigravity" and not native_adapter(selected):
                            selected = "agy"
                        native_binary(selected)
                        model = read_field("Native model (Enter for profile): ", required=False) or None
                        task = read_field("Native task (Enter for readiness): ", required=False) or None
                        main_owner(selected, model)
                        if not main_installed(selected, model):
                            raise ValueError(main_missing(selected, model))
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
            elif choice == "f":
                previous = finalize_selection(repo, provider, navigation, cache.get("state", {}))
            elif choice == "n":
                task_id = read_field("Task ID to retry saved delivery (may duplicate an uncertain send): ")
                delivered = retry_delivery(repo, task_id)
                previous = "app delivery: " + delivered["receipt"]["status"]
            elif choice != "9":
                from panel_keymap import control_keys
                raise ValueError("choose " + ", ".join(control_keys()))
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
