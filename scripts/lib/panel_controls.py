"""Explicit terminal and job controls for the panel; read-only until the user chooses an action.

Backends are the frozen shell and job APIs (open_terminal/list_terminals/focus_terminal/
read_terminal, run_job/list_jobs/job_logs/interrupt_job/stop_job/restart_job). They are
imported lazily and may be injected; a missing backend is reported as a partial listing.
Listing never reads logs or screens and never mutates. Every action re-reads the current
list and matches the selected id, kind and job generation before it calls the backend.
"""

import importlib
import os
import shlex
import sys
import textwrap
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from dashboard_projection import clean, display_width, fit, mapping  # noqa: E402

SHELL_BACKEND = "panel_terminals"
JOB_BACKEND = "panel_jobs"
MAX_ROWS = 500
MAX_TEXT = 32768
FINISHED = {"completed", "succeeded", "finished", "exited", "failed", "stopped", "cancelled"}
SHELL_CAPS = (("f", "focus"), ("r", "read"), ("x", "forget"))
JOB_CAPS = (("l", "logs"), ("i", "interrupt"), ("s", "stop"), ("r", "restart"), ("x", "forget"))
RESTART_NOTE = "Restart loses the current log: only the latest generation's log is kept."
FORGET_NOTE = "Forget permanently deletes this private history record and any retained job log."
TRUNCATED_NOTE = "Truncated: output is incomplete."


def backend(name):
    try:
        return importlib.import_module(name)
    except ImportError:
        return None


def clip(text, width):
    """Never wider than width, even when width is smaller than the ellipsis."""
    if width <= 0:
        return ""
    text, out, used = fit(text, width), [], 0
    for ch in text:
        used += display_width(ch)
        if used > width:
            break
        out.append(ch)
    return "".join(out)


def generation_of(value):
    return value if type(value) is int and value > 0 else None


def normalize(item, kind):
    item = mapping(item)
    ident = item.get("id")
    if not isinstance(ident, str) or not ident or item.get("kind", kind) != kind:
        return None
    caps = mapping(item.get("capabilities"))
    names = [name for _, name in (SHELL_CAPS if kind == "shell" else JOB_CAPS)]
    return {"kind": kind, "id": ident, "shown_id": clean(ident, 24),
            "label": clean(item.get("label"), 60) or "(unlabelled)",
            "state": clean(item.get("state"), 20) or "unknown",
            "generation": generation_of(item.get("generation")) if kind == "job" else None,
            "exit_code": item.get("exit_code") if isinstance(item.get("exit_code"), int) else None,
            "caps": {name: caps.get(name) is True for name in names}}


def collect(repo, shells=None, jobs=None):
    """Return (rows, notes); an unsupported or failing backend yields a note, not an error."""
    rows, notes = [], []
    for kind, api, noun, call in (
            ("shell", shells, "Shells", lambda api: api.list_terminals(repo)),
            ("job", jobs, "Jobs", lambda api: api.list_jobs(repo))):
        if api is None:
            notes.append("Partial listing: %s unsupported on this host." % noun.lower())
            continue
        try:
            listed = call(api)
        except Exception as error:
            notes.append("Partial listing: %s unavailable (%s)." % (noun.lower(), clean(str(error), 80)))
            continue
        if not isinstance(listed, list):
            notes.append("Partial listing: %s unavailable (unreadable list)." % noun.lower())
            continue
        found = [normalize(item, kind) for item in listed[:MAX_ROWS]]
        if None in found:
            notes.append("%d malformed %s rows ignored." % (found.count(None), noun.lower()))
        rows.extend(r for r in found if r)
    return rows, notes


def visible(rows, show_finished):
    return [r for r in rows if show_finished or r["kind"] != "job" or r["state"] not in FINISHED]


def row_line(number, row, width):
    extra = ""
    if row["kind"] == "job":
        extra = " g%s" % (row["generation"] if row["generation"] is not None else "?")
        if row["exit_code"] is not None:
            extra += " exit %d" % row["exit_code"]
    return clip("%2d %-5s %-10s %s%s" % (number, row["kind"], row["state"], row["label"], extra), width)


def render(rows, notes, page, width, height, show_finished):
    """Return (lines, page) for the clamped page; no ANSI, within width columns and height lines."""
    shown = visible(rows, show_finished)
    finished = sum(r["kind"] == "job" and r["state"] in FINISHED for r in rows)
    head = ["Terminals and jobs: %d shown, %d finished %s%s" % (
        len(shown), finished, "hidden (f shows)" if finished and not show_finished else
        "shown (f hides)" if show_finished else "hidden",
        " [partial]" if notes else "")] + list(notes)
    avail = height - 1
    cap = avail - (1 if avail >= 2 else 0)
    if len(head) > cap:
        head = head[:cap - 1] + ["%d more notes omitted" % (len(head) - cap + 1)] if cap >= 2 else head[:cap]
    size = avail - len(head)
    pages = max(1, -(-len(shown) // size)) if size > 0 else 1
    page = min(max(page, 0), pages - 1)
    lines = head + [row_line(i + 1, r, width) for i, r in enumerate(shown) if size > 0 and page * size <= i < (page + 1) * size]
    if not shown and size > 0:
        lines.append("No terminals or jobs.")
    lines.append("page %d/%d  n/p page, u refresh, f finished, o open shell, r run job, number select, Enter exit"
                 % (page + 1, pages))
    return [clip(line, width) for line in lines][:height], page


def safe_text(text, width, height, truncated=False):
    """Last lines of untrusted output: controls (including ESC) become '?'; within width and height."""
    lines = []
    for raw in text[-MAX_TEXT * 2:].splitlines():
        line = "".join(" " if ch == "\t" or unicodedata.category(ch)[0] == "Z"
                       else "?" if unicodedata.category(ch)[0] == "C" else ch for ch in raw)
        lines.append(clip(line, width))
    room = height - 1 if truncated else height
    lines = lines or ["(empty)"]
    if len(lines) > room:
        omitted = len(lines) - max(room - 1, 0)
        lines = ["... %d earlier lines omitted" % omitted] + lines[len(lines) - room + 1:] if room >= 1 else []
    if truncated:
        lines.insert(0, TRUNCATED_NOTE)
    return "\n".join(clip(line, width) for line in lines[:height])


def view_of(result):
    """(text, truncated) from a backend read; only an explicit True claims truncation."""
    if isinstance(result, str):
        return result, False
    result = mapping(result)
    text = result.get("text")
    return (text if isinstance(text, str) else ""), result.get("truncated") is True


def parse_command(line):
    try:
        argv = shlex.split(line)
    except ValueError as error:
        raise ValueError("Malformed command: %s" % clean(str(error), 60))
    if not argv:
        raise ValueError("Enter a command to run.")
    return argv


def browse_controls(repo, *, session=None, reader=input, writer=print, width=80, height=24,
                    shells=None, jobs=None):
    """Interactive loop; leaving it never stops a job or closes a shell."""
    shells = backend(SHELL_BACKEND) if shells is None else shells
    jobs = backend(JOB_BACKEND) if jobs is None else jobs
    width = width if type(width) is int and width > 0 else 80
    height = height if type(height) is int and height > 0 else 24
    page, show_finished = 0, False

    def put(text):
        writer("\n".join(clip(line, width) for line in text.splitlines()))

    def ask(prompt):
        return reader(prompt).strip()

    def fresh(sel):
        rows, _ = collect(repo, shells, jobs)
        for row in rows:
            if sel["kind"] == "job" and sel["generation"] is None:
                return None
            if (row["kind"], row["id"], row["generation"]) == (sel["kind"], sel["id"], sel["generation"]):
                return row
        return None

    def perform(sel, name):
        if sel["kind"] == "job" and sel["generation"] is None:
            return "Job generation is unknown; control refused."
        if name in {"restart", "forget"}:
            note = RESTART_NOTE if name == "restart" else FORGET_NOTE
            verb = "restart" if name == "restart" else "forget"
            if not session:
                if name == "restart":
                    return "Restart requires an explicit session."
            if len(textwrap.wrap(note, width)) > height:
                return "%s refused: display too small to show the deletion warning." % verb.title()
            put("\n".join(textwrap.wrap(note, width)))
            if ask("Type yes to %s: " % verb) != "yes":
                return "%s cancelled." % verb.title()
        current = fresh(sel)
        if current is None or not current["caps"].get(name):
            return "Selection is stale or the action is unavailable; reselect from the list."
        ident, gen = current["id"], current["generation"]
        if name == "focus":
            shells.focus_terminal(repo, ident)
            return "Focus requested."
        if name == "read":
            text, cut = view_of(shells.read_terminal(repo, ident, maximum=MAX_TEXT, lines=200))
            return safe_text(text, width, height, cut)
        if name == "logs":
            text, cut = view_of(jobs.job_logs(repo, ident, gen, maximum=MAX_TEXT))
            return safe_text(text, width, height, cut)
        if name == "interrupt":
            jobs.interrupt_job(repo, ident, gen)
            return "Interrupt requested."
        if name == "stop":
            jobs.stop_job(repo, ident, gen)
            return "Stop requested."
        if name == "forget":
            if sel["kind"] == "shell":
                shells.forget_terminal(repo, ident)
            else:
                jobs.forget_job(repo, ident, gen)
            return "Private history and any retained log deleted."
        jobs.restart_job(repo, session, ident, gen)
        return "Restart requested."

    def select(sel):
        caps = SHELL_CAPS if sel["kind"] == "shell" else JOB_CAPS
        offered = [(key, name) for key, name in caps if sel["caps"].get(name)]
        put("%s %s [%s]: %s" % (sel["kind"], sel["shown_id"], sel["state"], sel["label"]))
        shown_names = ["%s %s" % (key, "restart (loses log)" if name == "restart" else
                                  "forget (deletes history/log)" if name == "forget" else name) for key, name in offered]
        put(", ".join(shown_names) or "No actions available.")
        key = ask("Action (Enter to go back): ")
        if not key:
            return
        name = dict(offered).get(key)
        if name is None:
            put("Action not available.")
            return
        try:
            put(perform(sel, name))
        except Exception as error:
            put("Action failed: " + clean(str(error), 120))

    def launch(kind):
        if not session:
            return "Opening a shell or running a job requires an explicit session."
        api = shells if kind == "shell" else jobs
        if api is None:
            return "%s unsupported on this host." % ("Shells" if kind == "shell" else "Jobs")
        if kind == "shell":
            label = clean(ask("Label (Enter for Terminal): "), 60) or "Terminal"
            api.open_terminal(repo, session, label=label)
            return "Shell opened."
        argv = parse_command(ask("Command (split like a shell, run without one): "))
        label = clean(ask("Label (Enter for Job): "), 60) or "Job"
        api.run_job(repo, session, argv, label=label)
        return "Job started."

    while True:
        rows, notes = collect(repo, shells, jobs)
        lines, page = render(rows, notes, page, width, height, show_finished)
        put("\n".join(lines))
        try:
            cmd = ask("> ")
        except EOFError:
            return
        if cmd in ("", "q"):
            return
        shown = visible(rows, show_finished)
        if cmd == "n":
            page += 1
        elif cmd == "p":
            page -= 1
        elif cmd == "u":
            pass  # The loop re-lists every pass; this is the explicit, non-mutating refresh.
        elif cmd == "f":
            show_finished, page = not show_finished, 0
        elif cmd in ("o", "r"):
            try:
                put(launch("shell" if cmd == "o" else "job"))
            except Exception as error:
                put("Launch refused: " + clean(str(error), 120))
        elif cmd.isascii() and cmd.isdigit() and len(cmd) <= len(str(len(shown))) and 1 <= int(cmd) <= len(shown):
            try:
                select(shown[int(cmd) - 1])
            except EOFError:
                return
        else:
            put("Unknown command.")
