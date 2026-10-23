#!/usr/bin/env python3
"""Render deterministic, fictional OMS terminal surfaces as standalone SVGs."""
import argparse
import calendar
import html
import os
import re
import sys
import time
import unicodedata
from pathlib import Path

# Renderers consult both standard terminal variables and OMS-specific overrides.
os.environ.update({"TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "C.UTF-8",
                   "LC_ALL": "C.UTF-8", "COLUMNS": "120", "LINES": "34",
                   "COLORFGBG": "0;15", "OMS_PANEL_THEME": "dark",
                   "OMS_PANEL_COLOR": "always", "OMS_PANEL_UNICODE": "1"})
for variable in ("NO_COLOR", "OMS_PANEL_NO_COLOR", "OMS_PANEL_LIGHT", "OMS_PANEL_COMPACT"):
    os.environ.pop(variable, None)

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
import dashboard_projection
import panel_view
import room_view

OUT = Path(__file__).resolve().parent
WIDTH, HEIGHT = 120, 27
BOARD_HEIGHT = 28
FIXED_NOW = calendar.timegm(time.strptime("2026-10-23T04:00:00Z", "%Y-%m-%dT%H:%M:%SZ"))
_gmtime = time.gmtime
_strftime = time.strftime
time.time = lambda: FIXED_NOW
time.localtime = lambda value=None: _gmtime(FIXED_NOW if value is None else value)
time.strftime = lambda pattern, value=None: _strftime(pattern, time.localtime() if value is None else value)


def fixture():
    people = [
        ("main-1", "main", "claude-opus-5-5", "working", None, "Ship request validation"),
        ("builder-1", "builder", "gpt-6.1-sol", "working", "main-1", "Add input validation"),
        ("reviewer-1", "reviewer", "claude-fable-5-1", "done", "main-1", "Review validation patch"),
        ("verifier-1", "verifier", "gpt-6.1-sol", "failed", "main-1", "Verify acceptance"),
        ("guard-1", "worker", "gpt-6.1-sol", "blocked", "main-1", "Guard stopped unsafe edit"),
        ("done-1", "worker", "gpt-6.1-sol", "done", "main-1", "Update request tests"),
    ]
    participants, attempts = [], []
    for seq, (ident, role, model, state, parent, label) in enumerate(people, 1):
        participants.append({"participant": ident, "role": role, "joined": True,
                             "parent": parent, "model": model, "label": label, "seq": seq,
                             "guard": ident == "guard-1", "joined_at": "2026-10-23T03:57:00Z"})
        attempts.append({"attempt_id": ident, "state": state, "parent_attempt_id": parent,
                         "provider": "claude" if model.startswith("claude") else "codex",
                         "tool": "delegate", "task_id": ident,
                         "started_at": "2026-10-23T03:57:00Z",
                         "updated_at": "2026-10-23T03:57:00Z",
                         "ended": "2026-10-23T04:00:00Z" if state == "done" else None,
                         "exit": 1 if state == "failed" else 0 if state == "done" else None,
                         "exit_code": 1 if state == "failed" else 0 if state == "done" else None,
                         "verify_status": "failed" if role == "verifier" else "passed" if role == "reviewer" else None,
                         "verify_exit": 1 if role == "verifier" else 0 if role == "reviewer" else None,
                         "reason_code": "worker_authority_violation" if ident == "guard-1" else None,
                         "panel": {"role": role, "model": model, "label": label,
                                   "location": "/work/acme-webapp", "purpose": role,
                                   "room_participant": ident}})
    events = []
    texts = ["Starting request validation work", "Builder is checking input boundaries",
             "Validation rejects missing identifiers", "Verify passed: request checks are green",
             "Review verdict: PASS with high confidence", "Verifier failed: malformed input case uncovered",
             "Guard stopped an out-of-scope edit", "Land prepared for feature/validation",
             "Finalized request validation; next: oms panel"]
    for index, text in enumerate(texts, 1):
        events.append({"id": "event-%02d" % index, "sender": people[(index - 1) % len(people)][0],
                       "ts": "2026-10-20T12:%02d:00Z" % index, "text": text,
                       "message_kind": "status"})
    report = {"collection": {"ok": True}, "repo": {"name": "acme/webapp"},
              "goal": {"text": "Ship reliable request validation", "source": "task"},
              "plan": {"present": True, "goal": "Ship reliable request validation",
                       "task_count": 4, "by_state": {"running": 1, "review": 1, "blocked": 1, "done": 1},
                       "tasks": [{"id": "validation", "title": "Validate request input", "state": "running"},
                                 {"id": "review", "title": "Review patch", "state": "review"},
                                 {"id": "verify", "title": "Verify edge cases", "state": "blocked"},
                                 {"id": "tests", "title": "Add regression tests", "state": "done"}]},
              "room": {"id": "fictional-room", "participants": participants, "messages": events,
                       "statuses": {"main-1": {"text": "Checking request boundaries"}}},
              "attempts": {"available": True, "active_recent": attempts, "recent": []},
              "room_attempts": {a["attempt_id"]: {"attempt_id": a["attempt_id"], "state": a["state"],
                                                        "task_id": a["task_id"], "exit": a.get("exit"),
                                                        "verify_status": a.get("verify_status"),
                                                        "verify_exit": a.get("verify_exit")}
                                for a in attempts},
              "provider_status": {"claude": {"weekly_used": 28}, "codex": {"weekly_used": 46}},
              "attention": {"actionable": 2, "items": [
                  {"priority": "high", "code": "verification", "summary": "Repair verifier edge case", "command": "oms panel"},
                  {"priority": "medium", "code": "worker_review", "summary": "Confirm guard stop was expected", "command": "oms panel"}],
                  "omitted": 0, "next": []}}
    return report


def sgr_style(code, style):
    parts = code.split(";") if code else ["0"]
    i = 0
    while i < len(parts):
        try:
            n = int(parts[i] or 0)
        except ValueError:
            i += 1
            continue
        if n == 0:
            style.clear()
        elif n == 1:
            style["bold"] = True
        elif n == 2:
            style["dim"] = True
        elif n == 22:
            style.pop("bold", None); style.pop("dim", None)
        elif n in (39, 49):
            style.pop("fg" if n == 39 else "bg", None)
        elif 30 <= n <= 37 or 90 <= n <= 97:
            style["fg"] = ["#22262e", "#f07178", "#c3e88d", "#ffcb6b", "#82aaff", "#c792ea", "#89ddff", "#eeffff"][n % 10]
        elif 40 <= n <= 47 or 100 <= n <= 107:
            style["bg"] = ["#22262e", "#f07178", "#c3e88d", "#ffcb6b", "#82aaff", "#c792ea", "#89ddff", "#eeffff"][n % 10]
        elif n in (38, 48) and i + 1 < len(parts):
            key = "fg" if n == 38 else "bg"
            if parts[i + 1] == "5" and i + 2 < len(parts):
                try:
                    rgb = xterm(int(parts[i + 2]))
                    style[key] = "#%02x%02x%02x" % rgb
                except (ValueError, TypeError):
                    pass
                i += 2
            elif parts[i + 1] == "2" and i + 4 < len(parts):
                try:
                    rgb = tuple(int(x) for x in parts[i + 2:i + 5])
                    if all(0 <= channel <= 255 for channel in rgb):
                        style[key] = "#%02x%02x%02x" % rgb
                except (ValueError, TypeError):
                    pass
                i += 4
        i += 1


def xterm(n):
    n = max(0, min(255, n))
    if n < 16:
        colors = [(0,0,0),(205,0,0),(0,205,0),(205,205,0),(0,0,238),(205,0,205),(0,205,205),(229,229,229),
                  (127,127,127),(255,0,0),(0,255,0),(255,255,0),(92,92,255),(255,0,255),(0,255,255),(255,255,255)]
        return colors[n]
    if n < 232:
        n -= 16
        return tuple(0 if v == 0 else 55 + 40 * v for v in (n // 36, n // 6 % 6, n % 6))
    v = 8 + 10 * (n - 232)
    return v, v, v


def cell_width(ch):
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


def svg(terminal, title):
    lines = terminal.splitlines()
    cols = max((sum(cell_width(c) for c in re.sub(r"\033\[[0-9;]*m", "", line)) for line in lines), default=80)
    cols = min(max(cols, 80), 140)
    rows = max(len(lines), 1)
    cw, ch, top, pad = 9, 18, 48, 20
    width, height = cols * cw + 2 * pad, rows * ch + top + pad
    out = ['<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d" font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, &quot;DejaVu Sans Mono&quot;, monospace">' % (width, height, width, height),
           '<rect width="100%" height="100%" rx="14" fill="#11151c"/>',
           '<path d="M14 0h%da14 14 0 0 1 14 14v34H0V14A14 14 0 0 1 14 0z" fill="#252b36"/>' % (width - 28),
           '<circle cx="22" cy="23" r="5" fill="#f07178"/><circle cx="40" cy="23" r="5" fill="#ffcb6b"/><circle cx="58" cy="23" r="5" fill="#c3e88d"/>',
           '<text xml:space="preserve" x="%d" y="28" fill="#c6d0e0" font-family="sans-serif" font-size="13" style="white-space: pre">%s</text>' % (width // 2 - 100, html.escape(title))]
    for y, line in enumerate(lines):
        style, x = {}, 0
        pieces = re.split(r"(\033\[[0-9;]*m)", line)
        for piece in pieces:
            match = re.fullmatch(r"\033\[([0-9;]*)m", piece)
            if match:
                sgr_style(match.group(1), style)
                continue
            if not piece:
                continue
            safe = html.escape(piece)
            attrs = ['fill="%s"' % style.get("fg", "#c6d0e0")]
            if style.get("bold"):
                attrs.append('font-weight="700"')
            if style.get("dim"):
                attrs.append('opacity="0.62"')
            if style.get("bg"):
                out.append('<rect x="%d" y="%d" width="%d" height="%d" fill="%s"/>' % (pad+x*cw, top+y*ch, sum(map(cell_width, piece))*cw, ch, style["bg"]))
            out.append('<text xml:space="preserve" x="%d" y="%d" font-size="15px" style="white-space: pre" %s>%s</text>' % (pad+x*cw, top+y*ch+14, " ".join(attrs), safe))
            x += sum(map(cell_width, piece))
    out.append('</svg>')
    return "\n".join(out) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default=str(OUT), metavar="DIR", help="write SVGs to DIR")
    output = Path(parser.parse_args(argv).out)
    output.mkdir(parents=True, exist_ok=True)
    report = fixture()
    board = panel_view.render(report, "codex", WIDTH, BOARD_HEIGHT, color=True, view="tree", main_attempt="main-1")
    graph = room_view.render_graph(report, WIDTH, HEIGHT, color=True, main_attempt="main-1",
                                  navigation={"dismissed": True, "tab": "log"})
    results = panel_view.render_results({"rows": [
        {"task_id": "validation", "title": "Add request validation", "role": "builder", "purpose": "builder",
         "model": "gpt-6.1-sol", "process_state": "completed", "outcome": "accepted",
         "verification": {"status": "passed"},
         "summary": "Reject invalid values before processing. Regression coverage confirms missing and malformed identifiers fail safely.",
         "next_action": "oms panel", "review": {"status": "passed", "gate_verdict": "pass", "confidence": "high",
         "findings_summary": "No blocking findings; one low severity naming suggestion."},
         "calls": [{"kind": "verify", "exit": 0, "state": "done", "served_model": "gpt-6.1-sol",
                    "verify_command": "python3 -m pytest tests/test_requests.py", "verify_exit": 0,
                    "answer": "Verify command: python3 -m pytest tests/test_requests.py (exit 0, passed). Review gate verdict: PASS (confidence high). Findings: no blocking issues; one low severity naming suggestion. Next: oms panel."}]},
        {"task_id": "edge-cases", "title": "Verify malformed request handling", "role": "verifier", "purpose": "verifier",
         "model": "gpt-6-sol", "process_state": "failed", "verification": {"status": "failed"},
         "summary": "One malformed identifier still reaches the parser. Repair the validation branch, then rerun the verifier.",
         "next_action": "oms panel", "calls": [{"kind": "verify", "exit": 1, "state": "failed",
                    "served_model": "gpt-6-sol", "verify_command": "python3 -m pytest tests/test_requests.py -k malformed",
                    "verify_exit": 1, "answer": "Verify command failed: malformed identifier case remains uncovered. Awaiting owner decision after repair."}]}]}, WIDTH)
    dash = dashboard_projection.render({"repo": {"name": "acme/webapp", "branch": "feature/validation", "head": "abc1234"},
        "generated_at": "2026-10-20T12:00:00Z", "coverage": "complete", "collection": {"sources": {"task": "ok", "room": "ok"}},
        "task": {"healthy": True, "present": True, "task_id": "validation", "status": "active", "verification": "pending", "stale": False},
        "goal": {"text": "Ship reliable request validation", "source": "task"}, "scope": {"allowed": ["src/"], "forbidden": [], "source": "task"},
        "plan": {"healthy": True, "present": True, "task_count": 3, "by_state": {"running": 1, "review": 1, "done": 1}, "actionable": [], "stale_claims": 0, "contract_blocker": None},
        "attempts": {"available": True, "total": 4, "active": 2, "by_state": {"working": 1, "review": 1}, "recent": [], "active_recent": []},
        "delegations": [], "operations": {"available": True, "recent": [], "omitted": 0}, "reviews": {"outcomes": True, "passed": 2, "failed": 0, "unknown": 0, "seat_answers": 0},
        "acceptance": None, "attention": {"actionable": 2, "items": [{"priority": "high", "code": "worker_review", "summary": "Review validation patch", "command": "oms panel"},
        {"priority": "medium", "code": "verification", "summary": "Verify acceptance criteria", "command": "oms panel"}], "omitted": 0, "next": []}}, WIDTH, height=26)
    images = {"panel-board.svg": board, "panel-graph.svg": graph, "panel-results.svg": results, "dashboard.svg": dash}
    for name, text in images.items():
        rendered = svg(text, name[:-4].replace("-", " ").title())
        path = output / name
        with path.open("w", encoding="utf-8", newline="\n") as target:
            target.write(rendered)
        size = path.stat().st_size
        if size > 150 * 1024:
            raise SystemExit("oversize SVG: " + name)
        print("%s %d bytes" % (name, size))


if __name__ == "__main__":
    main()
