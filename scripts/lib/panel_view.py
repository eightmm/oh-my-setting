"""Bounded terminal presentation of the existing read-only dashboard contract."""

import os
import re
import stat
import unicodedata
from collections import OrderedDict
from datetime import datetime

from dashboard_projection import (clean, count, display_width, fit, listing, mapping, number,
                                  wrap_label as wrapped)
from panel_routing import policy
from call_ending import LIVE_STATES, call_ending



def light_theme():
    """OMS_PANEL_THEME=light|dark wins; else a light COLORFGBG background (7 or 15) means light."""
    chosen = os.environ.get("OMS_PANEL_THEME", "").lower()
    if chosen in ("light", "dark"):
        return chosen == "light"
    return os.environ.get("COLORFGBG", "").rpartition(";")[2] in ("7", "15")


ROLE_COLORS = {"main": (215, 130, 33), "judge": (141, 97, 35),
               "counsel": (111, 25, 34),
               "worker": (114, 28, 32), "unknown": (245, 242, 90)}
STATE_COLORS = {"running": (117, 24, 36), "good": (114, 28, 32),
                "alert": (221, 130, 33), "bad": (203, 160, 31), "dim": (245, 242, 90)}
TOKEN_COLORS = dict(ROLE_COLORS, **STATE_COLORS, metadata=(245, 242, 90), border=(240, 245, 90),
                    text=(None, None, 39))
# Compatibility tokens share semantic colours while older consumers migrate.
TOKEN_ALIASES = {"codex": "main", "review": "judge", "head": "text"}
GATE_ROLES = {"reviewer", "verifier", "security-auditor", "judge"}
COUNSEL_ROLES = {"advisor", "architect", "critic", "researcher", "council"}
JUDGE_ROLES = GATE_ROLES | COUNSEL_ROLES
WORKER_ROLES = {"worker", "builder", "refactorer", "documenter", "integrator",
                "performance-engineer", "release-engineer"}
GLYPHS = {"running": ("⟳", "~"), "spinner": ("⠋⠙⠹⠸", "|/-\\"),
          "gate": ("◆", "#"), "counsel": ("◇", "+"),
          "cancelled": ("⊘", "x"), "guard_stop": ("⛔", "!"), "timed_out": ("⏱", "!"),
          "verified": ("✓", "+"), "failed": ("✗", "!"),
          "waiting": ("!", "!"), "unknown": ("?", "?"), "idle": ("○", "o"),
          "selected": ("▸", ">"), "expanded": ("▾", "v"), "dot": ("•", "o"),
          "swatch": ("■", "#"), "separator": ("·", "/"), "ellipsis": ("…", "..."),
          "right": ("→", "->"), "up": ("↑", "^"), "down": ("↓", "v"),
          "top_left": ("╭", "+"), "top_right": ("╮", "+"), "bottom_left": ("╰", "+"),
          "bottom_right": ("╯", "+"), "horizontal": ("─", "-"), "vertical": ("│", "|"),
          "dashed_horizontal": ("╌", "- "), "dashed_vertical": ("╎", ":"),
          "branch": ("├", "+"), "endpoint": ("└", "`"), "junction_right": ("┤", "+"),
          "junction_down": ("┬", "+"), "junction_up": ("┴", "+"), "junction": ("┼", "+")}
_UNICODE = True


def use_unicode(flag):
    global _UNICODE
    from dashboard_projection import use_unicode as configure
    _UNICODE = bool(flag)
    configure(flag)


def glyph(name, unicode=True):
    return GLYPHS[name][0 if unicode else 1]


def role_style(role, purpose=None) -> str:
    chosen = purpose if purpose in JUDGE_ROLES | WORKER_ROLES else role
    return "main" if role == "main" else "judge" if chosen in JUDGE_ROLES else "worker" if chosen in WORKER_ROLES else "unknown"


def judge_band(role, purpose=None):
    chosen = purpose if purpose in JUDGE_ROLES | WORKER_ROLES else role
    if role != "main":
        return "gate" if chosen in GATE_ROLES else "counsel" if chosen in COUNSEL_ROLES else None


def band_style(role, purpose=None):
    return "counsel" if judge_band(role, purpose) == "counsel" else role_style(role, purpose)


def role_legend(unicode=True):
    return [(glyph("swatch", unicode) + " Main", "main"),
            (glyph("gate", unicode) + " Gate", "judge"),
            (glyph("counsel", unicode) + " Counsel", "counsel"),
            (glyph("swatch", unicode) + " Workers", "worker")]


_JUDGMENTS = OrderedDict()
_JUDGMENT_BYTES = 64 * 1024


def _repo_root(report):
    return os.path.realpath(mapping(report.get("repo")).get("path") or
                            os.environ.get("OMS_ROOM_REPO") or os.environ.get("OMS_PANEL_REPO") or os.getcwd())


def _artifact_judgment(report, member):
    """Read a regular OMS artifact's tail once per file generation; never open pipes or links."""
    relative = member.get("artifact")
    if (not isinstance(relative, str) or not relative.startswith(".oms/artifacts/") or
            "\\" in relative or ":" in relative or any(p in {"", ".", ".."} for p in relative.split("/"))):
        return None
    key = None
    try:
        root = _repo_root(report)
        path = os.path.join(root, relative)
        if os.path.realpath(path) != path:
            return None
        info = os.stat(path, follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode):
            return None
        key = (path, info.st_size, info.st_mtime_ns)
        if key in _JUDGMENTS:
            _JUDGMENTS.move_to_end(key)
            return _JUDGMENTS[key]
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(descriptor, "rb") as source:
            opened = os.fstat(source.fileno())
            if not stat.S_ISREG(opened.st_mode) or (opened.st_size, opened.st_mtime_ns) != key[1:]:
                return None
            source.seek(max(0, opened.st_size - _JUDGMENT_BYTES))
            data = source.read(_JUDGMENT_BYTES)
            after = os.fstat(source.fileno())
            if (after.st_size, after.st_mtime_ns) != key[1:]:
                return None
        text = data.decode("utf-8", "replace").replace("\r\n", "\n")
        if opened.st_size > _JUDGMENT_BYTES:
            text = text.partition("\n")[2]  # A partial leading line cannot state a verdict.
        # Peer artifacts wrap the answer between Output and Exit; metadata is not a judgment.
        if "## Output\n" in text:
            text = text.rsplit("## Output\n", 1)[1]
        text = text.split("## Exit", 1)[0]
        from terminal_panel import _review_verdict
        from panel_debate import _stance
        verdict = _review_verdict(text)
        findings = [0] * 4
        fence, prose = None, []
        for line in text.splitlines():
            marker = re.match(r"^(`{3,}|~{3,})", line.lstrip())
            if marker:
                if fence is None:
                    fence = marker[1]
                elif re.fullmatch(r" {0,3}" + re.escape(fence[0]) + "{%d,} *" % len(fence), line.expandtabs(4)):
                    fence = None
                continue
            if fence or line.lstrip().startswith(">"):
                continue
            prose.append(line)
            finding = re.match(r"^\s*(?:[-*+]\s+|\d+[.)]\s+|#{1,6}\s+)?(?:\*\*)?\[?P([0-3])\b", line, re.I)
            if finding:
                findings[int(finding[1])] += 1
        explicit = next((line for line in prose if re.match(
            r"(?i)^[\s*#_-]*(?:VERDICT|Recommendation)\b[\s*_]*[:\-]?", line)), None)
        counsel = _stance("\n".join(prose)) if explicit else (None, "")
        parsed = (verdict, findings, clean(counsel[0] or counsel[1], 400).lower(), bool(text.strip()))
        _JUDGMENTS[key] = parsed
        while len(_JUDGMENTS) > 256:
            _JUDGMENTS.popitem(last=False)
        return parsed
    except (OSError, ValueError, TypeError):
        if key is not None:
            _JUDGMENTS[key] = None
            while len(_JUDGMENTS) > 256:
                _JUDGMENTS.popitem(last=False)
        return None


def judgment_tag(member, report, unicode=True, fallback=None):
    fallback = fallback if fallback is not None else status_tag(member.get("state"), exit_code=member.get("exit"), unicode=unicode)
    band = judge_band(member.get("role"), member.get("purpose"))
    if not band or member.get("state") not in TERMINAL_STATES | {"review", "exited"}:
        return fallback
    parsed = _artifact_judgment(report, member)
    if parsed:
        gate, findings, counsel, answered = parsed
        if band == "gate" and gate:
            counts = "".join(" " + glyph("separator", unicode) + " P%s %s" % (i, n) for i, n in enumerate(findings) if n) if gate == "fail" else ""
            return "[%s gate %s%s]" % (glyph("verified" if gate == "pass" else "failed", unicode), gate, counts)
        if band == "counsel":
            if (member.get("purpose") or member.get("role")) == "researcher" and answered:
                return "[answered]"
            if counsel:
                return "[verdict %s]" % counsel
    code = member.get("exit")
    return "[%s no verdict (exit %s)]" % (glyph("failed", unicode), code) if type(code) is int and code != 0 else fallback


def judgment_tone(tag, state):
    if "guard stop" in tag:
        return "alert"
    if "gate fail" in tag or "no verdict" in tag:
        return "bad"
    if "gate pass" in tag or tag == "[answered]":
        return "good"
    return "counsel" if tag.startswith("[verdict ") else tone(state)


def ending_tag(call, unicode=True):
    return status_tag(call_ending(call) or call.get("state"), exit_code=call.get("exit"), unicode=unicode)


def call_tone(call):
    return tone(call_ending(call) or call.get("state"))


def call_identity(role, model, label=None, short_id=None, alias=None, purpose=None) -> str:
    # Routing stores a broad board kind in role and the requested judge/worker
    # role in purpose when that distinction is available.
    display_role = purpose if purpose in JUDGE_ROLES | WORKER_ROLES else role
    identity = ((clean(display_role) or "unknown").replace("-", " ").upper() + " " +
                (MODEL_NAMES.get(model, clean(model)) or "model unknown"))
    if alias:
        identity = clean(alias, 8) + " " + identity
    if short_id:
        identity += " #" + clean(short_id).lstrip("#")
    if label:
        identity += " " + glyph("separator", _UNICODE) + " " + clean(label)
    return identity


def status_tag(state, *, exit_code=None, unicode=True) -> str:
    if state in TERMINAL_STATES | {"exited"}:
        if state == "cancelled" or exit_code in {129, 130, 143}:
            state = "cancelled"
        elif state == "timed_out" or exit_code == 124:
            state = "timed_out"
    if state in {"cancelled", "guard_stop", "timed_out", "verify_failed", "verify_timed_out", "provider_error"}:
        mark = "failed" if state in {"verify_failed", "verify_timed_out", "provider_error"} else state
        words = {"verify_timed_out": "verify timed out"}.get(state, state.replace("_", " "))
        if state == "provider_error" and type(exit_code) is int:
            words += " " + glyph("separator", unicode) + " exit %s" % exit_code
        return "[%s %s]" % (glyph(mark, unicode), words)
    state = {"working": "running", "live marker": "running", "manual": "running", "waiting_input": "needs input",
             "waiting_approval": "needs approval", "review": "awaiting decision", "timed_out": "timed out",
             "orphaned": "lost process", "lost_process": "lost process", "unresolved": "status unknown",
             "unknown": "status unknown", "presence unknown": "status unknown", "exited": "finished",
             "done": "finished", "checks_passed": "checks passed", "checks_not_run": "checks not run",
             "checks_unknown": "checks unknown", "test_failed": "test failed",
             "acceptance_pending": "acceptance pending"}.get(state, state)
    mark = ("running" if state in {"starting", "running", "verifying", "loading"} else
            "idle" if state == "idle" else "waiting" if state in {"blocked", "needs approval", "awaiting decision"} else
            "unknown" if state in {"needs input", "status unknown"} else
            "failed" if state in {"failed", "timed out", "abandoned", "lost process", "unavailable"} else
            "verified" if state in {"finished", "checks passed", "accepted"} or re.fullmatch(r"\d+/\d+ pass", str(state)) else None)
    known = mark is not None or state in {"queued", "cancelled", "checks not run", "checks unknown",
                                          "acceptance pending", "test failed"}
    if not known:
        state, mark = "status unknown", "unknown"
    if state in {"finished", "failed", "test failed"} and type(exit_code) is int:
        state += ": exit %s" % exit_code
    return "[" + (glyph(mark, unicode) + " " if mark else "") + state + "]"


def paint(text, token, *, color=False, colors=16, light=False, bold=False) -> str:
    preference = os.environ.get("OMS_PANEL_COLOR", "").lower()
    if not color or preference in {"0", "no", "false", "never"} or "NO_COLOR" in os.environ:
        return text
    return _sgr(token, colors, light, bold) + text + "\033[0m"


def paint_card_edges(text, token, *, unicode=True, color=False, colors=16, light=False):
    edge = glyph("dashed_vertical", unicode)
    left, right = text.find(edge), text.rfind(edge)
    if not color or left < 0 or right <= left:
        return text
    styled = paint(edge, token, color=True, colors=colors, light=light)
    return text[:left] + styled + text[left + 1:right] + styled + text[right + 1:]


def _sgr(token, colors, light, bold):
    style = TOKEN_ALIASES.get(token, token)
    dark, bright, sgr = TOKEN_COLORS.get(style, TOKEN_COLORS["unknown"])
    code = "38;5;%s" % (bright if light else dark) if colors >= 256 and dark is not None else str(
        37 if light and style in {"unknown", "dim", "metadata"} else sgr)
    return "\033[" + ("1;" if bold else "") + code + "m"


def color_count():
    return 256 if "256color" in os.environ.get("TERM", "") else 16


class _Palette(dict):
    # An unstyled or unknown token renders plain rather than failing the frame.
    def __missing__(self, key):
        return ""


PALETTE = _Palette({name: _sgr(name, color_count(), light_theme(), name in {"main", "codex", "head"})
                    for name in list(TOKEN_COLORS) + list(TOKEN_ALIASES)})


def swap_line(room):
    swaps = listing(mapping(room).get("swaps"))
    if not swaps:
        return ""
    latest = mapping(swaps[-1])
    stamp = latest.get("ts", "")
    try:
        clock = datetime.fromisoformat(stamp.replace("Z", "+00:00")).astimezone().strftime("%H:%M")
    except (ValueError, TypeError):
        clock = "time unknown"
    new = MODEL_NAMES.get(latest.get("model"), latest.get("model")) or "model unknown"
    old = MODEL_NAMES.get(latest.get("from_model"), latest.get("from_model")) or "model unknown"
    return "MAIN %s · took over from %s at %s" % (new, old, clock)
PROVIDER_NAMES = {"codex": "Codex", "claude": "Claude Code", "pi": "Pi"}
TERMINAL_STATES = {"done", "failed", "cancelled", "timed_out", "abandoned"}
TASK_NAMES = {"peer-delegate": "Implement patch", "delegate": "Implement patch",
              "ask": "Investigate / advise", "consult": "Investigate / advise",
              "review": "Review changes", "peer-review": "Review changes"}
MODEL_NAMES = {"gpt-6.1-sol": "Sol 6.1", "gpt-6-sol": "Sol 6", "gpt-6-luna": "Luna 6", "gpt-6-astra": "Astra 6",
               "claude-opus-5-5": "Opus 5.5", "claude-sonnet-5-5": "Sonnet 5.5",
               "claude-fable-5-1": "Fable 5.1", "claude-haiku-5-5": "Haiku 5.5"}
PURPOSE_NAMES = {"implement": "Implement patch", "explain": "Explain code",
                 "investigate": "Inspect source", "review": "Review changes", "advise": "Advise decision",
                 "research": "Look up sources",
                 # Role strategies record their role as the purpose (routing keeps the four board kinds).
                 "builder": "Build", "refactorer": "Refactor", "documenter": "Document", "integrator": "Integrate",
                 "performance-engineer": "Measure performance", "release-engineer": "Prepare release",
                 "architect": "Structure design", "critic": "Find failure cases", "verifier": "Verify against request",
                 "security-auditor": "Security audit", "reviewer": "Review changes", "advisor": "Advise decision",
                 "researcher": "Look up sources", "worker": "Work"}
ATTENTION_STATES = {"blocked", "waiting_input", "waiting_approval", "review", "orphaned",
                    "failed", "timed_out", "abandoned"}


def activity(state, frame=None, unicode=True):
    if state in LIVE_STATES:
        return (glyph("spinner", unicode)[frame % 4]
                if frame is not None else glyph("running", unicode))
    return status_tag(state, unicode=unicode)[1:2] if state != "cancelled" else glyph("idle", unicode)


def location_label(value):
    if not value:
        return "location unrecorded"
    return value.replace("worktree:oh-my-setting-delegate.", "worktree ").replace("worktree:", "worktree ")


def main_occupied(report, members=None):
    """Return attempt-shaped evidence; joined mains need confirmed termination before replacement."""
    attempt = next((row for row in listing(mapping(report.get("attempts")).get("active_recent"))
                    if mapping(row.get("panel")).get("role") == "main"
                    and row.get("state") not in TERMINAL_STATES), None)
    if attempt is not None:
        return attempt
    if members is None:
        from room_view import nodes
        members = nodes(report)
    member = next((row for row in members if row.get("role") == "main"
                   and row.get("state") not in TERMINAL_STATES), None)
    if member is not None:
        return {"attempt_id": member.get("attempt"), "state": member.get("state"),
                "panel": {"role": "main", "room_id": mapping(report.get("room")).get("id"),
                          "room_participant": member.get("participant")}}
    return None


def status_alerts(report):
    alerts = []
    if mapping(report.get("collection")).get("ok") is not True:
        alerts.append("! Collection degraded - some state unknown")
    error = mapping(report.get("room")).get("error")
    if error:
        alerts.append("! Room: " + clean(error, 160) + " - o selects a room, 9 retries")
    if report.get("capacity_alerts"):
        alerts.extend(report["capacity_alerts"])
    return alerts


def clipped(text, width):
    # Preserve layout spaces; clean() deliberately collapses them in data labels.
    text = "".join(" " if ch in "\t\n\r" else "?" if unicodedata.category(ch)[0] == "C"
                   else ch for ch in str(text)[:2000] if ch not in "\ufe0e\ufe0f")
    if width >= 3:
        return fit(text, width)
    result = ""
    for char in text:
        if display_width(result + char) > width:
            break
        result += char
    return result


def fixed_column(text, width):
    """Clip and pad a label to a display-cell width."""
    width = max(0, int(width))
    value = clipped(text, width)
    return value + " " * max(0, width - display_width(value))


def message_rows(text, width, indent=None):
    """Preserve message line breaks and wrap by cells with a hanging continuation indent."""
    rows = []
    width = max(1, width)
    for line in str(text).splitlines():
        line = clipped(line, display_width(line))
        hanging = max(0, int(indent)) if indent is not None else 0
        if hanging > width // 2 or width - hanging < 2:
            hanging = 0
        value, cells = "", 0
        for char in line:
            step = display_width(char)
            if value and cells + step > width:
                rows.append(value)
                value, cells = " " * hanging, hanging
            value += char
            cells += step
        rows.append(value)
    return rows


def tone(state):
    if state == "cancelled":
        return "dim"
    if state == "guard_stop":
        return "alert"
    if state in {"timed_out", "verify_failed", "verify_timed_out", "provider_error"}:
        return "bad"
    tag = status_tag(state, unicode=False)
    if tag.startswith(("[! failed", "[! timed out", "[! abandoned", "[! lost process", "[! unavailable", "[test failed")):
        return "bad"
    if tag.startswith(("[! blocked", "[! needs approval", "[! awaiting decision", "[? needs input")):
        return "alert"
    if tag.startswith("[~"):
        return "running"
    return "good" if tag.startswith(("[+ checks passed", "[+ accepted")) or re.match(r"\[\+ \d+/\d+ pass\]", tag) else "dim"


def worker_rows(report):
    """Only explicit panel metadata identifies advisors and main ownership."""
    from room_view import call_classifier, nodes
    classify = call_classifier(report)
    projected = {member.get("attempt"): member for member in nodes(report) if member.get("attempt")}
    rows = []
    attempts = mapping(report.get("attempts"))
    active = list(listing(attempts.get("active_recent")))
    seen = {row.get("attempt_id") for row in active}
    active += [row for row in listing(attempts.get("recent"))
               if row.get("state") not in TERMINAL_STATES and row.get("attempt_id") not in seen]
    locations = {mapping(row.get("panel")).get("location") for row in active}
    for row in listing(report.get("delegations")):
        if str(row.get("task_id") or "").startswith("preflight-"):
            continue
        if row.get("location") and row["location"] in locations:
            continue
        state = "live marker" if row.get("live") else "orphaned"
        detail = "role: " + (row.get("role") or "unrecorded")
        model = "model: unrecorded"
        if row.get("requested_model"):
            model = "requested: " + row["requested_model"]
        if row.get("reasoning_effort"):
            model += " | effort: " + row["reasoning_effort"]
        rows.append({"state": state, "provider": row.get("provider") or "unknown", "work": detail,
                     "task": row.get("task_id") or "unrecorded", "model": model,
                     "name": MODEL_NAMES.get(row.get("requested_model"), row.get("requested_model")) or
                             "model unknown",
                     "role": "worker", "source": "marker", "parent": None,
                     "location": row.get("location"), "effort": row.get("reasoning_effort"), "access": None})
    # A running council owns its seats: same task and parent, shown as one debate.
    councils = {(row.get("task_id"), row.get("parent_attempt_id")) for row in active
                if row.get("tool") == "panel-council"}
    seats = {}
    for row in active:
        if str(row.get("task_id") or "").startswith("preflight-"):
            continue
        key = (row.get("task_id"), row.get("parent_attempt_id"))
        if key in councils and row.get("tool") != "panel-council":
            seats[key] = seats.get(key, 0) + 1
    for row in active:
        metadata = mapping(row.get("panel"))
        member = projected.get(row.get("attempt_id"))
        if member is not None and classify(member) == "past":
            continue
        key = (row.get("task_id"), row.get("parent_attempt_id"))
        if row.get("tool") == "panel-council":
            rows.append({"state": row.get("state") or "unknown", "provider": row.get("provider") or "unknown",
                         "work": "Debate: " + (metadata.get("label") or "council"),
                         "task": row.get("task_id") or "unrecorded", "model": "4 seats / 2 families",
                         "name": "%s seat(s) answering" % seats.get(key, 0),
                         "role": "council", "source": "council", "parent": row.get("parent_attempt_id"),
                         "location": None, "effort": None, "access": None})
            continue
        if metadata.get("role") == "main":
            continue
        detail = TASK_NAMES.get(row.get("tool"), row.get("tool") or "Work unrecorded")
        operation = next((item for item in listing(mapping(report.get("operations")).get("recent"))
                          if row.get("attempt_id") and item.get("attempt_id") == row["attempt_id"]), {})
        model = ("served: " + operation["served_model"] if operation.get("served_model") else
                 "selected: " + operation["selected_model"] if operation.get("selected_model") else
                 "selected: " + metadata["model"] if metadata.get("model") else
                 "model: unrecorded")
        model_id = operation.get("served_model") or operation.get("selected_model") or metadata.get("model")
        if metadata.get("fallback"):
            model += " | fallback: " + metadata["fallback"]
        rows.append({"state": row.get("state") or "unknown",
                     "provider": metadata.get("provider") or row.get("provider") or "unknown",
                     "work": "Seat answering" if key in councils else
                             metadata.get("label") or PURPOSE_NAMES.get(metadata.get("purpose"), detail),
                     "task": row.get("task_id") or "unrecorded", "model": model,
                     "name": MODEL_NAMES.get(model_id, model_id) or "model unknown",
                     "role": "council" if key in councils else metadata.get("role") or "worker",
                     "purpose": metadata.get("purpose"), "attempt": row.get("attempt_id"),
                     "tokens": call_tokens(report, row.get("attempt_id"), row.get("state")),
                     "joined_at": row.get("started_at") or row.get("created_at"), "exit": row.get("exit_code"),
                     "source": "attempt",
                     "parent": row.get("parent_attempt_id") if metadata.get("role") else None,
                     "location": metadata.get("location"),
                     "effort": metadata.get("effort"), "access": metadata.get("access"),
                     "owner": metadata.get("owner")})
    rank = {"failed": 0, "orphaned": 0, "blocked": 1, "waiting_approval": 1, "waiting_input": 1}
    return sorted(rows, key=lambda row: (row["source"] != "council", rank.get(row["state"], 2)))


def hierarchy(report, rows, main_attempt=None, include_empty=True):
    """Show the live main's calls; historical owners remain in lineage and Past."""
    attempts = mapping(report.get("attempts"))
    mains = {a.get("attempt_id"): a for a in listing(attempts.get("recent")) +
             listing(attempts.get("active_recent")) if a.get("attempt_id")
             and mapping(a.get("panel")).get("role") == "main"}
    from room_view import nodes
    projected = {m["attempt"]: m for m in nodes(report)
                 if m.get("role") == "main" and m.get("attempt")}
    live = [ident for ident, attempt in mains.items()
            if attempt.get("state") and attempt["state"] not in TERMINAL_STATES]
    current = main_attempt if main_attempt in mains else next(iter(live), None)

    def owner(row):
        # Calls of any other recorded main surface as unlinked so none is hidden.
        parent = row.get("parent")
        return None if parent in mains and parent != current else parent

    roots = []
    parents = list(dict.fromkeys(owner(r) for r in rows))
    if include_empty and current and current not in parents:
        parents.append(current)
    for ident in parents:
        children = [r for r in rows if owner(r) == ident]
        main = mains.get(ident, {})
        metadata = mapping(main.get("panel"))
        model = projected[ident].get("model") if ident in projected else metadata.get("model")
        title = (call_identity("main", model) if main else
                 "LINKED OWNER / metadata unavailable" if ident else "No known main")
        kinds = list(dict.fromkeys(r.get("purpose") if r.get("purpose") in JUDGE_ROLES | WORKER_ROLES else r["role"] for r in children))
        groups = [(key.upper() + ("" if key == "council" else "S"), sorted(
            [r for r in children if (r.get("purpose") if r.get("purpose") in JUDGE_ROLES | WORKER_ROLES else r["role"]) == key],
            key=lambda r: r["state"] not in LIVE_STATES)) for key in kinds]
        groups.sort(key=lambda group: role_style(group[1][0]["role"], group[1][0].get("purpose")) != "judge")
        roots.append({"id": ident, "title": title, "label": metadata.get("label"),
                      "state": main.get("state"), "groups": [(g, rs) for g, rs in groups if rs]})
    if main_attempt:
        return [r for r in roots if r["id"] == main_attempt]
    return roots


def model_usage(report):
    """Group the retained call artifacts once; lifecycle totals overlap them."""
    groups, seen = {}, set()
    for row in listing(mapping(report.get("operations")).get("recent")):
        if row.get("kind") not in {"ask", "call", "consult", "review", "delegate"}:
            continue
        ident = row.get("event_id")
        if ident and ident in seen:
            continue
        if ident:
            seen.add(ident)
        ambiguous = row.get("model_attribution") == "ambiguous" or row.get("fallback_used") is True
        attribution = "mixed" if ambiguous else "served" if row.get("served_model") else "selected" if row.get("selected_model") else "unknown"
        model = None if ambiguous else row.get("served_model") or row.get("selected_model")
        key = (row.get("provider"), model, attribution)
        item = groups.setdefault(key, {"name": "Mixed models" if ambiguous else MODEL_NAMES.get(model, model) or
            PROVIDER_NAMES.get(row.get("provider"), "Unknown"), "attribution": attribution,
            "records": 0, "tokens": 0, "token_reports": 0, "cost": 0, "cost_reports": 0})
        item["records"] += 1
        tokens = provider_usage_tokens(mapping(row.get("provider_usage")))
        if tokens is None:
            tokens = count(row.get("tokens"))
        cost = number(row.get("cost_usd"))
        if tokens is not None:
            item["tokens"] += tokens
            item["token_reports"] += 1
        if cost is not None:
            item["cost"] += cost
            item["cost_reports"] += 1
    return list(groups.values())


def amount(value):
    return (str(value // 1000000) + "." + str(value % 1000000 // 100000) + "m" if value >= 1000000 else
            str(value // 1000) + "." + str(value % 1000 // 100) + "k" if value >= 10000 else str(value))


def provider_usage_tokens(usage):
    """Return a comparable provider total when its input/output/cache contract is complete."""
    parts = [count(usage.get(key)) for key in ("input_tokens", "output_tokens")]
    if usage.get("cache_in_input") is False:
        parts += [count(usage.get(key)) for key in ("cache_read_tokens", "cache_write_tokens")]
    elif usage.get("cache_in_input") is not True:
        return None
    return sum(parts) if None not in parts else None


def call_tokens(report, attempt_id, state, short=False):
    """One call's tokens: providers differ on cache, so a breakdown is summed to the same total."""
    if state in LIVE_STATES:
        return "tok pending" if short else "tokens pending"
    operation = next((item for item in listing(mapping(report.get("operations")).get("recent"))
                      if attempt_id and item.get("attempt_id") == attempt_id), {})
    usage = mapping(operation.get("provider_usage"))
    tokens = provider_usage_tokens(usage)
    if tokens is not None:
        text = amount(tokens) + " tok"
        return text if short else text + " (out %s)" % amount(count(usage.get("output_tokens")))
    tokens = count(operation.get("tokens"))
    if tokens is None:
        tokens = next((count(item.get("tokens")) for item in listing(mapping(report.get("attempts")).get("recent"))
                       if attempt_id and item.get("attempt_id") == attempt_id), None)
    if tokens is None:
        return "tok n/a" if short else "tokens unreported"
    return amount(tokens) + " tok"


def usage_labels(report, costs=False):
    labels = []
    for row in model_usage(report):
        tokens = amount(row["tokens"]) if row["token_reports"] else "?"
        if 0 < row["token_reports"] < row["records"]:
            tokens += "+?"
        suffix = "(sel)" if row["attribution"] == "selected" else "(?)" if row["attribution"] == "unknown" else ""
        label = "%s%s %s tok / %sx" % (row["name"], suffix, tokens, row["records"])
        if costs:
            cost = "$%.4g" % row["cost"] if row["cost_reports"] and number(row["cost"]) is not None else "$?"
            if 0 < row["cost_reports"] < row["records"]:
                cost += "+?"
            label += " / " + cost
        labels.append(label)
    return labels


def box_edge(title, width, unicode=True, kind="top", dashed=False):
    left, right, bar = (("╭", "╮", "─") if kind == "top" else
                        ("├", "┤", "─") if kind == "divider" else ("╰", "╯", "─")) if unicode else ("+", "+", "-")
    if dashed:
        bar = glyph("dashed_horizontal", unicode)
    label = " " + clipped(title, max(1, width - 6)) + " " if title else ""
    fill = max(0, width - display_width(label) - 3)
    return left + bar[0] + label + (bar * fill)[:fill] + right


def box_row(text, width, unicode=True, dashed=False):
    edge = glyph("dashed_vertical" if dashed else "vertical", unicode)
    text = clipped(text, width - 4)
    return edge + " " + text + " " * max(0, width - display_width(text) - 3) + edge


def menu_rows(width, height, unicode=True, expanded=False, managed=False, keys=True, occupied=False, installed=None):
    """Starting a main, rooms, refresh and quit; the board's own keys are not repeated."""
    quit_label = "Detach" if managed else "Quit"
    other = ("[s] Swap main  " if occupied else "") + "[o] Rooms  [k] Capacity  [9] Refresh  [?] More  [q] " + quit_label
    harnesses = [("claude", "[1] Claude"), ("pi", "[2] Pi"), ("codex", "[0] Codex")]
    start_keys = [label for name, label in harnesses if installed is None or name in installed]
    starts = "  ".join(start_keys + (["[3] Resume", "[t] Task"] if start_keys else []))

    def wrap_groups(text, row_width, limit):
        groups = [group.strip() for group in re.findall(r"\[[^]]+\](?: [^[]+)?", text)]
        if not groups:
            return wrapped(text, row_width, limit)
        rows, current = [], ""
        for group in groups:
            candidate = group if not current else current + " " + group
            if current and display_width(candidate) > row_width:
                rows.append(current)
                current = group
            else:
                current = candidate
        if current:
            rows.append(current)
        return [clipped(line, row_width) for line in rows[:limit]]
    if height < 20 or width < 28:
        options = (["[s] Swap main [o] Rooms [k] Capacity [9] Refresh [?] [q] " + quit_label,
                    "[s] [o] [k] [9] [?] [q]"] if occupied else
                   [" ".join(start_keys + ["[?]", "[q] " + quit_label]),
                    " ".join(re.findall(r"\[[^]]+\]", " ".join(start_keys)) + ["[?]", "[q]"])])
        compact = next((line for line in options if display_width(line) <= width), "[?] [q]")
        return [clipped(compact, width)]
    groups = ([] if occupied else [("START", starts)] if start_keys else []) + [("OTHER", other)]
    if expanded:
        groups += [("OPEN", "[h] Chats  [r] Results  [g] Graph  [z] Panel"),
                   ("TERMINAL", "[j] Terminals and jobs"),
                   ("WORK", "[5] Explain  [6] Implement  [e] Researcher  [a] Advisor  [7] Review  [c] Council"),
                   ("SETUP", "[4] Main provider  [p] CLIs  [8] Dashboard"),
                   ("RESULT", "[f] Finalize  [n] Retry delivery")]
    mode = "all shortcuts" if expanded else "press a key" if keys else "key + Enter"
    lines = [box_edge("Actions" + (" · " if unicode else " / ") + mode, width, unicode)]
    if width < 60 and not expanded:
        compact = ([] if occupied else ["  ".join(start_keys), "[3] Resume  [t] Task"] if start_keys else []) + [other]
        for line in compact:
            lines += [box_row(part, width, unicode) for part in wrap_groups(line, width - 4, 3)]
        return lines + [box_edge("", width, unicode, "bottom")]
    indent = 9 if expanded else 7
    for label, keys_text in groups:
        for index, line in enumerate(wrap_groups(keys_text, max(1, width - indent - 5), 5 if not expanded else len(keys_text))):
            lines.append(box_row((label.ljust(indent) if index == 0 else " " * indent) + line, width, unicode))
    return lines + [box_edge("", width, unicode, "bottom")]


INBOX_LIMIT = 12
FAILED_WORDS = {"failed": "failed", "timed_out": "timed out", "abandoned": "abandoned", "orphaned": "lost its process"}
WAITING_WORDS = {"waiting_approval": "waiting for approval", "waiting_input": "waiting for input"}
ONBOARDING = "Press {starts} to start the sole main{codex}"


def age_label(stamp, now=None):
    import calendar
    import time
    try:
        then = calendar.timegm(time.strptime(str(stamp)[:19], "%Y-%m-%dT%H:%M:%S"))
    except ValueError:
        return ""
    seconds = max(0, int((time.time() if now is None else now) - then))
    return ("<1m" if seconds < 60 else "%sm" % (seconds // 60) if seconds < 3600
            else "%sh" % (seconds // 3600) if seconds < 86400 else "%sd" % (seconds // 86400))


def inbox_items(report, unicode=True):
    """What the person must act on, newest first: failures, waits, broadcasts from mains, patches to admit."""
    from room_view import (DECIDED, call_classifier, closed_tasks, failure_handled, handled_results, main_names, main_state,
                           message_address, nodes, readable)
    members = nodes(report)
    classify = call_classifier(report)
    titles = main_names(members)
    name = lambda m: MODEL_NAMES.get(m.get("model"), m.get("model")) or m.get("provider") or "unknown"
    mains = {ident: m for m in members if m.get("role") == "main"
             for ident in (m["participant"], m.get("attempt")) if ident}
    attempts = mapping(report.get("attempts"))
    stamps = {row.get("attempt_id"): row.get("updated_at") for key in ("recent", "active_recent")
              for row in listing(attempts.get(key))}
    finalized, awaiting = mapping(report.get("finalized")), set(listing(report.get("awaiting_admission")))
    read_results, closed = handled_results(mapping(report.get("room"))), closed_tasks(report)
    successors = mapping(report.get("message_successors"))
    byid = {m["participant"]: m for m in members}
    label = lambda m: clean(titles.get(m["participant"]) or name(m))
    address = lambda ident: message_address(ident, members, successors,
                                           lambda target: label(byid[target]) if target in byid else clean(target))

    def who(member):
        if member.get("role") == "main":
            return titles.get(member["participant"], name(member)) + " main"
        parent = mains.get(member.get("parent"))
        return "%s %s" % (name(member), member.get("role")) + (
            " for " + titles.get(parent["participant"], name(parent)) if parent else "")
    items = []
    for member in members:
        state = main_state(member) if member.get("role") == "main" else member["state"]
        ending = call_ending(member)
        if ending == "cancelled":
            continue
        if ending in {"guard_stop", "timed_out", "verify_failed", "verify_timed_out", "provider_error"} and not failure_handled(report, member, read_results, closed):
            what, shown, style, kind = ending.replace("_", " "), ending, call_tone(member), ending
        elif state in FAILED_WORDS and not failure_handled(report, member, read_results, closed):
            what, shown, style, kind = FAILED_WORDS[state], state, "bad", "failed"
        elif state in WAITING_WORDS:
            what, shown, style, kind = WAITING_WORDS[state], state, "alert", "needs"
        elif state == "done" and member["participant"] in awaiting and classify(member) != "past":
            what, shown, style, kind = "patch awaits admission", "review", "review", "decide"
        elif state == "review" and classify(member) != "past" and finalized.get(member.get("task_id")) not in DECIDED:
            what, shown, style, kind = "awaiting decision", "review", "review", "decide"
        else:
            continue
        title = clean(member.get("title"), 80)
        items.append({"stamp": stamps.get(member.get("attempt")) or "", "glyph": activity(shown, None, unicode),
                      "who": who(member), "what": what + (": " + title if title and title != "Native task not recorded" else ""),
                      "style": style, "action": ("chat" if member.get("role") == "main" else "result", member["participant"]),
                      "row_id": ("call", member["participant"]), "cls": kind, "task": member.get("task_id")})
    from panel_messages import exited_mail
    open_ids = {mapping(q).get("id") for q in listing(mapping(report.get("room")).get("open_questions"))}
    for index, message in enumerate(listing(mapping(report.get("room")).get("messages"))):
        sender = mains.get(message.get("sender"))
        if (sender and message.get("recipient") == "all" and message.get("message_kind") != "answer"
                and not message.get("answered") and message.get("pending_for")):
            text = readable(message.get("text"), " ")
            items.append({"stamp": message.get("ts") or "", "glyph": glyph("right", unicode),
                          "who": who(sender), "what": "to all: " + clean(text, 80),
                          "style": "alert", "action": ("chat", sender["participant"]),
                          "row_id": ("message", str(message.get("id")) if message.get("id") else ("position", index)),
                          "cls": "needs" if message.get("message_kind") == "question" else "mail"})
    room = mapping(report.get("room"))
    pending = room.get("pending_messages") if "pending_messages" in room else room.get("messages")
    for index, message in enumerate(listing(pending)):
        if exited_mail(message, byid) and message.get("id") not in open_ids:
            target = successors.get(message["recipient"], message["recipient"])
            asker = byid.get(message.get("sender"), {})
            items.append({"stamp": message.get("ts") or "", "glyph": "?" if message.get("message_kind") == "question" else ">",
                          "who": ("Question" if message.get("message_kind") == "question" else "Message") +
                                 " from " + (label(asker) if asker else clean(message.get("sender"))),
                          "what": "to " + address(message["recipient"]) + ": " + clean(readable(message.get("text"), " "), 80),
                          "style": "alert", "action": ("chat", target),
                          "row_id": ("message", str(message.get("id")) if message.get("id") else ("position", index)),
                          "cls": "needs" if message.get("message_kind") == "question" else "mail"})
    import room as room_module
    for index, question in enumerate(listing(mapping(report.get("room")).get("open_questions"))):
        question, minutes = mapping(question), room_module.age(mapping(question).get("ts")) // 60
        asker, target = byid.get(question.get("sender")), byid.get(question.get("recipient"))
        if (asker and target and target.get("role") == "main"
                and (minutes * 60 >= room_module.PERSON_AFTER or target.get("state") in TERMINAL_STATES)):
            recipient = successors.get(target["participant"], target["participant"])
            items.append({"stamp": question.get("ts") or "", "glyph": "?", "who": "Question from " + label(asker),
                          "what": "to " + address(target["participant"]), "style": "alert",
                          "action": ("chat", recipient),
                          "row_id": ("question", str(question.get("id")) if question.get("id") else ("position", index)),
                          "cls": "needs"})
    failure_statuses = {"failed", "timeout", "timed_out", "error", "rejected", "unavailable", "failed_command",
                        "rpc_error", "persistence_failure", "absent", "archived", "busy", "invalid_timeout", "uncertain",
                        "unsupported_platform", "unbound", "ambiguous", "unsafe_endpoint", "empty_message", "oversized"}
    receipts = [*mapping(room.get("publications")).values(), *mapping(room.get("predecessor_publications")).values()]
    failed = [mapping(receipt) for receipt in receipts if mapping(receipt).get("status") in failure_statuses]
    if failed:
        items.append({"stamp": max(receipt.get("ts") or "" for receipt in failed), "glyph": "!",
                      "who": "Main wake-up delivery failed",
                      "what": "(unconfirmed)" if any(receipt.get("delivery_unknown") for receipt in failed) else "",
                      "style": "bad", "action": None, "row_id": ("delivery", "wake-up"), "cls": "info"})
    backlog = mapping(report.get("review_backlog"))
    from review_backlog import warning
    for participant, total in mapping(backlog.get("counts")).items():
        if total > backlog.get("threshold", 8) and participant in byid:
            # Undated and ahead of other undated rows, so it leads the decide class, which waits longest first.
            items.insert(0, {"stamp": "", "glyph": "!", "who": who(byid[participant]),
                             "what": warning(total), "style": "review", "action": ("chat", participant),
                             "row_id": ("review-backlog", participant), "cls": "decide"})
    return sorted(items, key=lambda item: item["stamp"], reverse=True)


# Attention classes in rank order: (key, header and row tag, narrow header word, tab heading).
ATTENTION_CLASSES = (("needs", "needs you", "you", "Needs you"), ("failed", "failed", "fail", "Failed"),
                     ("guard_stop", "guard stop", "guard", "Guard stops"),
                     ("timed_out", "timed out", "timeout", "Timed out"),
                     ("verify_failed", "verify failed", "verify", "Verify failed"),
                     ("verify_timed_out", "verify timed out", "verify timeout", "Verify timed out"),
                     ("provider_error", "provider error", "provider", "Provider errors"),
                     ("decide", "decide", "dec", "Awaiting decision"), ("stale", "stale", "stale", "Stale holds"),
                     ("mail", "mail", "mail", "Mail"), ("info", "info", "info", "Other"))


def attention_items(report, unicode=True):
    """The Attention queue: the inbox plus held tasks whose workers all ended, deduplicated and ranked by class.

    Within a class the newest comes first, except decisions, which wait longest first. Display-only."""
    from room_view import held_split
    items = inbox_items(report, unicode)
    failing = {item["task"] for item in items if item["cls"] in
               {"failed", "guard_stop", "timed_out", "verify_failed", "verify_timed_out", "provider_error"} and item.get("task")}
    tasks = {t.get("id"): t for t in map(mapping, listing(mapping(report.get("plan")).get("tasks")))}
    for ident in sorted(held_split(report)[1] - failing):
        title = clean(tasks.get(ident, {}).get("title"), 80)
        # An unresolved failure already names the task; the stale row would only repeat it.
        items.append({"stamp": str(tasks.get(ident, {}).get("claimed_at") or ""), "glyph": "○" if unicode else "o",
                      "who": "Task " + clean(ident, 40), "what": "workers ended, still held" + (": " + title if title else ""),
                      "style": "alert", "action": ("tab", "plan"), "row_id": ("task", ident), "cls": "stale"})
    seen, ranked = set(), []
    for kind, unused, unused_short, unused_heading in ATTENTION_CLASSES:
        group = sorted((item for item in items if item["cls"] == kind), key=lambda item: item["stamp"],
                       reverse=kind != "decide")
        for item in group:
            if item["row_id"] not in seen:
                seen.add(item["row_id"])
                ranked.append(dict(item, age=age_label(item["stamp"]) if kind == "decide" and item["stamp"] else ""))
    return ranked


def attention_header(ranked, width, unicode):
    """`Attention N · 1 needs you · 2 failed`, zero classes dropped; narrower widths shorten the class words."""
    sep = " · " if unicode else " / "
    counts = [(sum(item["cls"] == kind for item in ranked), tag, short) for kind, tag, short, unused in ATTENTION_CLASSES]
    for form in ("tag", "short", None):
        text = "Attention %s" % len(ranked) + "".join(
            sep + "%s %s" % (n, tag if form == "tag" else short) for n, tag, short in counts if n and form)
        if display_width(text) <= width:
            return text
    return clipped("Attn %s" % len(ranked), width)


def render_inbox(report, width, height, color, unicode, managed, previous, navigation):
    """The control window's menu pane: only what needs the person, then the few actions that start work."""
    navigation = navigation if navigation is not None else {}
    sep = " · " if unicode else " / "
    room = mapping(report.get("room"))
    items = inbox_items(report, unicode)
    occupied = bool(main_occupied(report) or room.get("error") or mapping(report.get("collection")).get("ok") is not True
                    or report.get("capacity_stale") or navigation.get("capacity_stale") or navigation.get("room_poll_unknown"))
    navigation["main_occupied"] = occupied
    footer = menu_rows(width, height, unicode, navigation.get("menu_help", False), managed,
                       keys=not navigation.get("line_input"), occupied=occupied, installed=navigation.get("installed"))
    if managed and height >= 20:
        footer.append("F5 Back / F12 keys / Ctrl-b d detach")
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    if len(footer) > height - 1 - 4:
        footer = menu_rows(width, 1, unicode, managed=managed, occupied=occupied, installed=navigation.get("installed"))
    budget = max(0, height - 1 - len(footer))
    head = ["OMS" + sep +
            (clean(room.get("title") or room.get("id")) or mapping(report.get("repo")).get("name") or "repository")]
    styles = ["head"]
    from panel_metrics import usage_line
    from room_view import goal_banner
    head.append(usage_line(report, width, unicode))
    styles.append("dim")
    full = goal_banner(report, width, unicode)
    # Header, usage and goal box are pinned; the goal box shrinks to one row before any of them is dropped.
    for text, style in full if height >= 20 and budget - 2 - len(full) >= 6 else goal_banner(report, width, unicode, True):
        head.append(text)
        styles.append(style)
    pinned = len(head)
    for alert in status_alerts(report):
        head.append(alert)
        styles.append("bad")
    if report.get("swap_state"):
        head.append("SWAP / " + clean(report["swap_state"], 80))
        styles.append("alert")
    if room.get("swaps") and height >= 20:
        head.append("SESSION / " + swap_line(room))
        styles.append("main")
    if previous is not None and height >= 12:
        last = message_rows("Last: " + previous, width) if "\n" in previous or "oms panel --release-main " in previous else ["Last: " + previous]
        if len(last) > 1:
            keep = max(pinned, budget - 3 - len(last))
            head, styles = head[:keep], styles[:keep]
            # Recovery commands must survive the optional-header trimming below.
            pinned = len(head) + len(last)
        head.extend(last)
        styles.extend(["dim"] * len(last))
    if not occupied:
        installed = navigation.get("installed")
        names = [(name, label) for name, label in (("claude", "1 (Claude)"), ("pi", "2 (Pi)"))
                 if installed is None or name in installed]
        starts = " or ".join(label for _, label in names)
        codex = "; 0 opens Codex" if installed is None or "codex" in installed else ""
        onboarding = ONBOARDING.format(starts=starts, codex=codex) if starts else ""
        protected = onboarding.replace("1 (Claude)", "1\u00a0(Claude)").replace("2 (Pi)", "2\u00a0(Pi)")
        tokens = re.findall(r"1\u00a0\(Claude\)|2\u00a0\(Pi\)|\S+", protected)
        onboarding_rows, current = [], ""
        for token in tokens:
            candidate = token if not current else current + " " + token
            if current and display_width(candidate) > width:
                onboarding_rows.append(current)
                current = token
            else:
                current = candidate
        if current:
            onboarding_rows.append(current)
        for line in onboarding_rows[:3] if onboarding else []:
            line = clipped(line.replace("\u00a0", " "), width)
            head.append(line)
            styles.append("main")
    keep = max(pinned, budget - 3)
    head, styles = head[:keep], styles[:keep]
    boxed = width >= 28 and budget - len(head) >= 4
    available = max(0, budget - len(head) - (2 if boxed else 0))
    shown, hidden = items[:INBOX_LIMIT], max(0, len(items) - INBOX_LIMIT)
    # Rows can share a destination. Keep the selection on the row while actions still name the destination.
    seen = {}
    row_ids = []
    for item in shown:
        key = item["row_id"]
        occurrence = seen.get(key, 0)
        seen[key] = occurrence + 1
        row_ids.append((key, occurrence))
    selected = navigation.get("selected")
    selected_row = navigation.get("selected_row")
    if selected_row not in row_ids or selected != shown[row_ids.index(selected_row)]["action"]:
        selected_row = next((row_ids[n] for n, item in enumerate(shown) if item["action"] == selected),
                            row_ids[0] if shown else None)
    selected = navigation["selected"] = shown[row_ids.index(selected_row)]["action"] if selected_row else None
    navigation["selected_row"] = selected_row
    capacity = available - (1 if len(shown) + bool(hidden) > available else 0) if shown else 1
    capacity = max(0, capacity)
    index = row_ids.index(selected_row) if selected_row else 0
    start = min(max(0, index - capacity + 1), max(0, len(shown) - capacity))
    window = shown[start:start + capacity]
    content_width = width - 4 if boxed else width
    body = []
    if not shown:
        body.append(("Nothing needs you right now", "dim", None))
    for row_id, item in zip(row_ids[start:start + capacity], window):
        tail = item["tail"] if "tail" in item else age_label(item["stamp"])
        tail = sep + tail if tail else ""
        text = clipped("%s %s%s" % (item["glyph"], item["who"], sep + item["what"] if item["what"] else ""),
                       max(1, content_width - display_width(tail) - 2)) + tail
        body.append((text, item["style"], item["action"], row_id))
    more = len(shown) - len(window) + hidden
    if shown and more and available > len(window):
        body.append(("+%s more%s" % (more, sep + "arrows, wheel or page keys" if len(shown) > len(window) else ""), "dim", None))
    lines, hits = [], []
    for text, style in zip(head, styles):
        lines.append(clipped(text, width))
        lines[-1] = (PALETTE[style] + lines[-1] + "\033[0m") if color else lines[-1]
    if boxed:
        title = "Needs you" + (sep + "%s" % len(items) if items else "") + (sep + "Up/Down, Enter opens" if items and not navigation.get("line_input") else "")
        lines.append((PALETTE["alert" if items else "dim"] if color else "") + box_edge(title, width, unicode) + ("\033[0m" if color else ""))
    for entry in body:
        text, style, action = entry[:3]
        row_id = entry[3] if len(entry) > 3 else None
        chosen = row_id is not None and row_id == selected_row
        value = clipped(text, content_width)
        if not color and chosen:
            value = clipped(value, max(0, content_width - 2)) + " <"
        if boxed:
            value = box_row(value, width, unicode)
        if color and chosen:
            value = PALETTE[style] + "\033[7m" + value + "\033[0m"
        elif color:
            value = PALETTE[style] + value + "\033[0m"
        lines.append(value)
        if action is not None:
            hits.append({"y": len(lines), "x1": 1, "x2": width, "action": action, "row_id": row_id})
    if boxed:
        lines.append(box_edge("", width, unicode, "bottom"))
    lines += [clipped(line, width) for line in footer]
    navigation.update(hits=hits, items=[item["action"] for item in shown], row_ids=row_ids,
                      offset=0, viewport=capacity,
                      surface="inbox",
                      room_id=room.get("id"), positions={}, geometry=(width, height, True, False))
    return "\n".join(lines[:height - 1])


def call_groups(groups, width, slots, compact, frame, unicode, limit):
    """Reserve each role's separator before distributing title space."""
    result, shown = [], 0
    content = max(0, slots - len(groups))
    for index, (name, records) in enumerate(groups):
        share = content // (len(groups) - index)
        rows = []
        wide = compact and width >= 80
        minimum = 1 if wide else 2 if compact else 3
        target = min(len(records), share // minimum, limit - shown)
        remaining = share
        for offset, row in enumerate(records[:target]):
            reserve = (target - offset - 1) * minimum
            title_limit = 1 if compact else min(3 if width < 54 else 2, remaining - reserve - 2)
            meta = "[%s] %s%s" % (row["state"], row["name"], " / " + row["access"] if row["access"] else "")
            if wide:
                meta = clipped(meta, 32)
                title_width = width - 8 - display_width(meta)
                text = clipped(row["work"], title_width)
                rows.append(("%s %s%s  %s" % (activity(row["state"], frame, unicode), text,
                    " " * max(0, title_width - display_width(text)), meta), tone(row["state"])))
                remaining -= 1
                shown += 1
                continue
            title = wrapped(row["work"], width - 6, max(1, title_limit))
            rows.extend(("%s %s" % (activity(row["state"], frame, unicode), line) if n == 0 else "  " + line, None)
                        for n, line in enumerate(title))
            rows.append(("  " + meta, tone(row["state"])))
            if not compact:
                detail = ("@ " + location_label(row["location"]) + " / " + (row["effort"] or "auto")
                          if row["location"] else row["model"])
                if width >= 70:
                    detail += " | task: " + row["task"]
                rows.append(("  " + detail, "dim"))
            remaining -= len(title) + (1 if compact else 2)
            shown += 1
        omitted = len(records) - target
        title = "%s (%s)%s" % (name, len(records), " / +%s hidden" % omitted if omitted else "")
        result.append((box_edge(title, width, unicode, "divider"), "worker" if name == "WORKERS" else "review"))
        result.extend((box_row(text, width, unicode), style) for text, style in rows)
        content -= len(rows)
    return result, shown


def room_unreadable(report):
    """Match only the no-room fallback used when collection fails."""
    return mapping(report.get("collection")) == {"ok": False} and not report.get("room")


def render_unavailable(report, width, height, color, unicode, previous, menu, managed, navigation):
    """Bounded status view for a room whose first read has failed."""
    # Nothing from the prior room remains selectable or clickable.
    for key in ("selected", "primary", "preview", "surface", "bands", "band_offsets", "box",
                "full_result", "graph_selected", "main_attempt", "body_rows", "preview_signature", "geometry",
                "selected_row", "row_ids", "detail"):
        navigation.pop(key, None)
    navigation.update(hits=[], items=[], positions={}, offset=0, viewport=0)
    budget = height - int(menu)
    if budget <= 0:
        return ""
    navigation["main_occupied"] = True
    footer = menu_rows(width, height, unicode, navigation.get("menu_help", False), managed,
                       keys=not navigation.get("line_input"), occupied=True) if menu else []
    if menu and managed and height >= 20:
        footer.append("F5 Back / F12 keys / Ctrl-b d detach")
    if navigation.get("notice"):
        footer.append(clean(navigation["notice"], 200))
    if len(footer) > budget - 1:
        footer = menu_rows(width, 1, unicode, managed=managed, occupied=True) if menu else []
    alert = "! Room state unavailable" if width >= 24 else "! Room state unknown" if width >= 20 else "! Unknown" if width >= 9 else "?"
    head = [("OMS" + (" · " if unicode else " / ") +
             (clean(mapping(report.get("repo")).get("name")) or "repository"), "head"),
            ("! " + clean(mapping(report.get("room")).get("error"), 160) if mapping(report.get("room")).get("error") else alert, "bad"),
            ("This room could not be read; its work and messages are unknown, not empty.", "dim"),
            (("o selects a room, 9 retries" if menu else "A later refresh may recover it."), "dim")]
    body = max(1, budget - len(footer))
    if previous is not None and height >= 12:
        last = message_rows("Last: " + previous, width) if "\n" in previous or "oms panel --release-main " in previous else ["Last: " + previous]
        head = head[:max(2, body - len(last))] + [(line, "dim") for line in last]
    if body == 1:
        head = [head[1] if width >= 20 else ("?", "bad")]
    lines = []
    for text, style in head[:body] + [(line, "dim" if line.startswith("F5") else "main") for line in footer]:
        text = clipped(text, width)
        lines.append(PALETTE[style] + text + "\033[0m" if color else text)
    return "\n".join(lines[:budget])


def needs_strip(report, width, unicode, limit=3, ranked=None):
    """Pinned queue counts; the complete ranked queue stays in Attention."""
    ranked = attention_items(report, unicode) if ranked is None else ranked
    if not ranked:
        return []
    rows = [{"text": attention_header(ranked, width, unicode), "style": ranked[0]["style"], "action": ("tab", "needs")}]
    return rows


def render(report, provider, width=100, height=28, color=False, unicode=True,
           previous=None, availability=None, menu=False, managed=False, main_attempt=None, frame=None,
           view="auto", attention_only=False, navigation=None):
    width, height = max(1, min(200, width)), max(1, height)
    use_unicode(unicode)
    color = bool(paint("x", "text", color=color) != "x")
    if navigation is not None:
        navigation.update(tab_strip_visible=False, tab_body_visible=False, help_rows=max(4, height // 3))
    if room_unreadable(report):
        return render_unavailable(report, width, height, color, unicode, previous, menu, managed,
                                  navigation if navigation is not None else {})
    if navigation is not None:
        # Graph-only targets must not leak into a tree drawn from the same navigation.
        for key in ("surface", "bands", "primary"):
            navigation.pop(key, None)
    if menu and view != "graph":
        return render_inbox(report, width, height, color, unicode, managed, previous, navigation)
    if view in ("tree", "summary") or (view == "auto" and mapping(report.get("room")).get("participants")
                          and width >= 20 and height >= 12):
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation, summary=view == "summary", managed=managed,
                           previous=previous)
    if view == "graph" and width >= 76 and height >= 16:
        from room_view import render_graph
        return render_graph(report, width, height, color, unicode, frame, menu, attention_only,
                            main_attempt, navigation, managed, previous=previous)
    if (view == "graph" and navigation is not None and mapping(report.get("room")).get("participants")
            and width >= 20 and height >= 8):
        from panel_tree import render_tree
        return render_tree(report, width, height, color, unicode, frame, menu, attention_only,
                           main_attempt, navigation, managed=managed, previous=previous)
    lines = []

    def add(text="", style=None):
        value = clipped(text, width)
        if color and style:
            value = PALETTE[style] + value + "\033[0m"
        lines.append(value)

    footer = []
    if menu:
        menu_navigation = navigation if navigation is not None else {}
        occupied = bool(
            menu_navigation.get("room_poll_unknown") or menu_navigation.get("capacity_stale") or report.get("capacity_stale")
            or mapping(report.get("room")).get("error") or mapping(report.get("collection")).get("ok") is not True
            or main_occupied(report))
        menu_navigation["main_occupied"] = occupied
        footer = menu_rows(width, height, unicode, menu_navigation.get("menu_help", False), managed,
                           keys=not menu_navigation.get("line_input"), occupied=occupied, installed=menu_navigation.get("installed"))
        if managed:
            footer.append("F5 control / F12 keys / Ctrl-b d detach")
    sep = " · " if unicode else " / "
    wide = "A finished call is not accepted until its main records it" + sep + "Claude Code's own agents are shown read-only"
    if width >= 28:
        footer.extend([wide if display_width(wide) <= width else "Finished is not accepted" + sep + "native agents are read-only"]
                      if width >= 54 else ["Finished is not accepted", "Native agents are read-only"])
    if height < 20 and width >= 28:
        footer = (footer[:1] if menu else []) + ["Finished is not accepted"]
    alerts = status_alerts(report)
    footer_space = max(0, height - int(menu) - 1 - len(alerts))
    footer = footer[-footer_space:] if footer_space else []
    budget = max(0, height - len(footer) - int(menu))
    repo = mapping(report.get("repo"))
    add("OMS" + sep + (os.path.basename(clean(repo.get("name")) or str(repo.get("path") or "").rstrip("/\\")) or "repository"), "head")
    from panel_metrics import usage_line
    from room_view import goal_banner
    add(usage_line(report, width, unicode), "dim")
    for text, style in goal_banner(report, width, unicode, compact=height < 20):
        add(text, style)
    for alert in alerts:
        add(alert, "bad")
    if height >= 34:
        add(repo.get("branch") or "branch unknown", "dim")

    attempts = mapping(report.get("attempts"))
    all_rows = worker_rows(report)
    other = sum(1 for row in all_rows if main_attempt and row["parent"] != main_attempt)
    rows = [row for row in all_rows if not main_attempt or row["parent"] == main_attempt]
    hidden = sum(1 for row in rows if attention_only and row["state"] not in ATTENTION_STATES)
    if attention_only:
        rows = [row for row in rows if row["state"] in ATTENTION_STATES]
    roots = hierarchy(report, rows, main_attempt, include_empty=not attention_only)
    main = next((row for row in listing(attempts.get("active_recent")) + listing(attempts.get("recent"))
                 if main_attempt and row.get("attempt_id") == main_attempt), {})
    metadata = mapping(main.get("panel"))
    from room_view import main_state, main_words, nodes
    projected_calls = {m.get("attempt"): m for m in nodes(report) if m.get("attempt")}
    shown_mains = {ident: m for ident, m in projected_calls.items() if m.get("role") == "main"}
    if main_attempt and not roots:
        roots = [{"id": main_attempt, "title": "MAIN / " + PROVIDER_NAMES.get(provider, provider),
                  "label": metadata.get("label"), "state": main.get("state"), "groups": []}]

    labels = usage_labels(report, costs=height >= 40)
    usage_title = "USAGE / recent 8 repo records"
    if width >= 80 and height < 30:
        strip = "USAGE recent8 / "
        used = 0
        for index, label in enumerate(labels):
            suffix = " / +%s models" % (len(labels) - index - 1) if index < len(labels) - 1 else ""
            candidate = strip + (" | " if used else "") + label
            if display_width(candidate + suffix) > width:
                break
            strip = candidate
            used += 1
        if used < len(labels):
            strip += " / +%s models" % (len(labels) - used)
        elif not labels:
            strip += "Usage unavailable" if mapping(report.get("operations")).get("available") is False else "unreported"
        add(strip, "dim")
    elif width >= 28 and height >= 24:
        add(box_edge(usage_title, width, unicode), "dim")
        cap = 3 if height >= 30 else 1
        packed, used = [], 0
        for label in labels:
            text = clipped(label, width - 4)
            if packed and display_width(packed[-1] + " | " + text) <= width - 4:
                packed[-1] += " | " + text
            elif len(packed) < cap:
                packed.append(text)
            else:
                break
            used += 1
        if not packed:
            packed = ["Usage unavailable" if mapping(report.get("operations")).get("available") is False
                      else "No reported usage"]
        if used < len(labels):
            # Keep the model count visible rather than clipping it after a subtotal.
            tail = "+%s models / recent 8 records" % (len(labels) - used)
            packed.append(tail)
        for line in packed:
            add(box_row(line, width, unicode))
        add(box_edge("", width, unicode, "bottom"), "dim")
    else:
        add("USAGE: " + (labels[0] if labels else "unreported"), "dim")

    room = mapping(report.get("room"))
    if room and height >= 20:
        narrow = width < 28
        add(("Room: " if narrow else "ROOM / ") + (clean(room.get("title") or room.get("id"), 160) or "unavailable")
            + (" (continues %s)" % clean(room["predecessor"], 160) if room.get("predecessor") else ""), "main")
        if room.get("swaps"):
            add(("Swap: " if narrow else "SESSION / ") + swap_line(room), "main")
        if report.get("swap_state"):
            add("SWAP / " + clean(report["swap_state"], 80), "alert")
        from room_view import main_names, nodes
        members = nodes(report)
        mains = [member for member in members if member.get("role") == "main"]
        if mains:
            live_mains = [member for member in mains if member.get("state") not in TERMINAL_STATES]
            if live_mains:
                labels = main_names(members)
                live_main = next((member for member in live_mains if main_attempt and main_attempt in
                                  (member.get("participant"), member.get("attempt"))), None)
                # Without an owner, the working main wins over a joined predecessor whose presence is unknown.
                live_main = live_main or next((member for member in live_mains if member.get("state") == "working"),
                                              live_mains[0])
                add(("Main " if narrow else "Main / ") + labels[live_main["participant"]], "dim")
            else:
                add(("Main " if narrow else "Main: none live (") + ("0 live / %s exited" % len(mains) if narrow else "%s exited)" % len(mains)), "dim")
        if view == "graph" and not narrow:
            add("Graph needs 76x16 / showing list", "dim")
        add(("Unread %s" % room.get("pending_count", "?") if narrow else
             sep.join(["Messages: %s unread" % room.get("pending_count", "?"), "%s read" % room.get("received_count", "?"),
                       "%s answered" % room.get("answered_count", "?")])), "dim")
    if not main_attempt:
        preset = policy()["main"].get(provider, {})
        if not any(root.get("id") for root in roots):
            add(("Next: " + PROVIDER_NAMES.get(provider, provider) if width < 28 else "NEXT MAIN / %s / %s / %s (preset)" % (
                PROVIDER_NAMES.get(provider, provider), MODEL_NAMES.get(preset.get("model"), preset.get("model")) or "native",
                preset.get("effort") or "native settings")), "main")
        if not roots:
            add("Goal: " + (mapping(report.get("goal")).get("text") or "No goal recorded"))

    acceptance = mapping(report.get("acceptance"))
    counts = mapping(acceptance.get("counts"))
    if acceptance.get("available"):
        evidence = ("Accept " if width < 28 else "REPO ACCEPT  ") + "%s/%s%s" % (counts.get("verified", 0), acceptance.get("total", "?"), " ok" if width < 28 else " verified")
        extras = "  ".join("%s=%s" % (key, counts[key]) for key in ("failed", "missing", "stale") if counts.get(key))
        if extras:
            evidence += "  " + extras
        evidence_tone = "bad" if counts.get("failed") else "good" if acceptance.get("complete") else "alert"
    else:
        evidence, evidence_tone = ("Accept unknown" if width < 28 else "REPO ACCEPT  evidence unavailable"), "alert"
    status_lines = [(evidence, evidence_tone)]
    items = sorted(listing(mapping(report.get("attention")).get("items")), key=lambda row: row.get("priority") or "P9")
    if items and height >= (30 if menu else 24):
        status_lines.append(("Repo: " + (items[0].get("summary") or "Attention required"), "alert"))
    if previous is not None and height >= 24:
        status_lines.append(("Last: " + previous, "dim"))
    if availability is not None and width >= 54 and height >= 34:
        status_lines.append(("CLI  " + "  ".join("%s: %s" % (name, "installed" if ready else "missing")
                                              for name, ready in availability.items()), "dim"))
    available = max(0, budget - len(lines) - len(status_lines) - 2)
    minimum = sum(2 + len(root["groups"]) + (3 if main_attempt else int(bool(root.get("label")))) for root in roots)
    compact = view == "compact" or (view == "auto" and minimum + 3 * len(rows) > available)
    add("OMS activity / " + ("attention" if attention_only else "this main" if main_attempt else "repository") +
        " / " + ("compact" if compact else "detail"), "alert" if attention_only else "dim")
    space = max(0, budget - len(lines) - len(status_lines) - 1)
    roots.sort(key=lambda r: (r["id"] is None, r["id"] or ""))
    all_records = [record for root in roots for _, group in root["groups"] for record in group]
    token_labels = {}
    for record in all_records:
        label = call_tokens(report, record.get("attempt"), record.get("state"), True)
        token_labels[record.get("attempt")] = "" if label in {"tok pending", "tok n/a"} else label
    token_width = max([0] + [display_width(label) for label in token_labels.values() if label])
    age_values = [age_label(record.get("joined_at")) or "unknown" for record in all_records]
    age_values += [main_words(shown_mains[root.get("id")], unicode)
                   if root.get("id") in shown_mains else "unknown" for root in roots]
    age_width = max([3] + [display_width(age) for age in age_values])
    tag_width = max([0] + [display_width(judgment_tag(projected_calls.get(record.get("attempt")), report, unicode,
                                                       ending_tag(projected_calls.get(record.get("attempt")), unicode)))
                            if projected_calls.get(record.get("attempt")) else
                            display_width(status_tag(record["state"], exit_code=record.get("exit"), unicode=unicode))
                            for record in all_records] + [
                                display_width(judgment_tag(main, report, unicode, ending_tag(main, unicode)))
                                for main in shown_mains.values()])
    shown = 0
    selected = (navigation or {}).get("selected")

    def list_row(identity, state, style, action=None, age="unknown", details=None, exit_code=None, connector="", call=None, attempt=None,
                 extras=()):
        nonlocal space
        if space <= 0:
            return
        tag = judgment_tag(call, report, unicode, ending_tag(call, unicode)) if call else status_tag(state, exit_code=exit_code, unicode=unicode)
        tag_tone = judgment_tone(tag, call_ending(call) or state) if call else tone(state)
        token_text = token_labels.get(attempt, "")
        row_token_width = token_width
        fixed = tag_width + 2 + age_width + (row_token_width + 2 if row_token_width else 0)
        if row_token_width and width - display_width(connector) - 4 < fixed + 1:
            row_token_width = 0
        tag_pad = " " * max(0, tag_width - display_width(tag))
        age_pad = " " * max(0, age_width - display_width(age))
        token_pad = " " * max(0, row_token_width - display_width(token_text)) if row_token_width else ""
        token_part = token_pad + token_text + "  " if row_token_width else ""
        tail = tag_pad + tag + "  " + token_part + age_pad + age
        tail_width = display_width(tail)
        available_tail = width - display_width(connector) - 4
        tiny_tail = False
        if tail_width > available_tail:
            if available_tail >= display_width(tag) + 2 + display_width(age):
                tail, tag_pad = tag + "  " + age, ""
            else:
                tail = clipped(tag + "  " + age, max(0, available_tail))
                tiny_tail = True
            tail_width = display_width(tail)
        identity = clipped(identity, max(0, available_tail - tail_width))
        gutter = glyph("selected", unicode) + " " if action and action == selected else "  "
        lead = gutter + connector + identity
        gap = " " * max(2, width - display_width(lead + tail))
        if action and action == selected and space >= 4 and width >= 28:
            add(box_edge(identity, width, unicode, dashed=True), style)
            state_line = box_row(tag + "  " + age, width, unicode, dashed=True)
            state_line = state_line.replace(tag, paint(tag, tag_tone, color=color,
                                                      colors=color_count(), light=light_theme()), 1)
            lines.append(paint_card_edges(state_line, style, unicode=unicode, color=color,
                                          colors=color_count(), light=light_theme()))
            kept = (details or [])[:max(0, space - 3)]
            for detail in kept:
                lines.append(paint_card_edges(box_row(detail, width, unicode, dashed=True), style,
                                              unicode=unicode, color=color, colors=color_count(), light=light_theme()))
            add(box_edge("", width, unicode, "bottom", dashed=True), style)
            space -= 3 + len(kept)
        else:
            value = clipped(lead + gap + tail, width)
            if color:
                prefix = (paint(gutter, "text", color=True, colors=color_count(), light=light_theme()) +
                          paint(connector + identity, style, color=True, colors=color_count(), light=light_theme(), bold=True) + gap)
                if tiny_tail:
                    value = prefix + paint(tail, "metadata", color=True, colors=color_count(), light=light_theme())
                else:
                    tag_start = len(tag_pad)
                    value = (prefix + paint(tail[:tag_start], "metadata", color=True, colors=color_count(), light=light_theme()) +
                             paint(tag, tag_tone, color=True, colors=color_count(), light=light_theme()) +
                             paint(tail[tag_start + len(tag):], "metadata", color=True, colors=color_count(), light=light_theme()))
            lines.append(value)
            space -= 1

    for root in roots:
        if space <= 0:
            break
        records = [row for _, group in root["groups"] for row in group]
        councils = {(r["task"], r["parent"]) for r in records if r["source"] == "council"}
        records = [r for r in records if r["source"] == "council" or r["role"] != "council" or (r["task"], r["parent"]) not in councils]
        judges = sorted([r for r in records if role_style(r["role"], r.get("purpose")) == "judge"], key=lambda r: r["state"] not in LIVE_STATES)
        workers = sorted([r for r in records if role_style(r["role"], r.get("purpose")) != "judge"], key=lambda r: r["state"] not in LIVE_STATES)
        # Reserve the main and one worker before fitting the judges above it.
        judge_count = min(len(judges), max(0, space - 1 - int(bool(workers))))

        def child(row):
            role = row.get("purpose") if row.get("purpose") in JUDGE_ROLES | WORKER_ROLES else row["role"]
            connector = "    " + glyph("branch", unicode) + glyph("dashed_horizontal", unicode)[0] + " "
            if frame is not None and row["state"] in LIVE_STATES:
                point = 1 + frame % 2
                connector = connector[:point] + glyph("dot", unicode) + connector[point + 1:]
            tokens = row.get("tokens")
            list_row(call_identity(role, row["name"], row["work"]), row["state"], band_style(row["role"], row.get("purpose")),
                     ("result", row.get("attempt")), age_label(row.get("joined_at")) or "unknown",
                     [row["work"], location_label(row.get("location")), row["model"]] + ([tokens] if tokens else []), row.get("exit"),
                     connector, call=projected_calls.get(row.get("attempt")), attempt=row.get("attempt"),
                     extras=())
        for row in judges[:judge_count]:
            child(row)
            shown += 1
        main = shown_mains.get(root["id"])
        title = root["title"] + (sep + root["label"] if root.get("label") else "")
        list_row(title, main_state(main) if main else root["state"], "main" if root["id"] else "unknown",
                 ("chat", main["participant"] if main else root["id"]),
                 main_words(main, unicode) if main else "unknown",
                 [root.get("label") or "No task recorded"])
        for row in workers[:max(0, space - int(len(records) > shown + space))]:
            child(row)
            shown += 1
        if not records and space:
            add("No active calls need attention" if attention_only else "No active child calls", "dim")
            space -= 1
    if not roots and space > 0:
        add("No active calls need attention" if attention_only and attempts.get("available") else
            "No active records" if attempts.get("available") else "Worker state unavailable", "dim")
    if len(rows) > shown or other or hidden:
        parts = ["+%s more" % (len(rows) - shown)] if len(rows) > shown else []
        parts += ["%s filtered" % hidden] if hidden else []
        parts += ["%s outside" % other] if other else []
        overflow = " / ".join(parts) + " / v Expand"
        if display_width(overflow) > width:
            parts = ["+%s" % (len(rows) - shown)] if len(rows) > shown else []
            parts += ["f%s" % hidden] if hidden else []
            parts += ["out%s" % other] if other else []
            overflow = " / ".join(parts) + " / v Expand"
        add(overflow, "dim")

    recent = [row for row in listing(attempts.get("recent")) if row.get("state") in TERMINAL_STATES
              and (not attention_only or row.get("state") in ATTENTION_STATES)
              and (not main_attempt or row.get("parent_attempt_id") == main_attempt or row.get("attempt_id") == main_attempt)]
    remaining = budget - len(lines) - len(status_lines)
    if remaining >= 2 and recent:
        add("Recent" if width < 28 else "RECENT / not acceptance", "dim")
        for row in recent[:min(2, remaining - 1)]:
            operation = next((item for item in listing(mapping(report.get("operations")).get("recent"))
                              if row.get("attempt_id") and item.get("attempt_id") == row["attempt_id"]), {})
            data = mapping(row.get("panel"))
            model = operation.get("served_model") or operation.get("selected_model") or data.get("model")
            source = "served" if operation.get("served_model") else "selected"
            work = (data["role"] + ": " + PURPOSE_NAMES.get(data.get("purpose"), row.get("tool") or "work")
                    if data else TASK_NAMES.get(row.get("tool"), row.get("tool") or "work"))
            add("%s %s / %s%s" % (status_tag(row.get("state"), exit_code=row.get("exit_code"), unicode=unicode), row.get("provider") or "unknown", work,
                  " / %s: %s" % (source, model) if model else ""), tone(row.get("state")))
    if len(lines) > max(0, budget - len(status_lines)):
        # Tiny panes prioritize summaries; no bordered card is emitted there.
        lines = lines[:max(0, budget - len(status_lines))]
    for text, style in status_lines[:budget]:
        add(text, style)
    lines = lines[:budget]
    for line in footer:
        add(line, "dim" if line.startswith(("OMS", "Native", "tmux", "A finished", "Finished", "Some")) else "main")
    return "\n".join(lines[:height - int(menu)])

def _result_blocks(text):
    """Classify the reader's list/code/pipe-table subset; retain original lines."""
    source = str(text).split("\n")
    containers, leaf, fence = [], "prose", ""
    paragraph = False
    delimiter_row = -1
    next_nonblank = 0

    def opener(value):
        match = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", value)
        if match and not (match[1][0] == "`" and "`" in match[2]):
            return match[1]
        return ""

    def item(value):
        match = re.match(r"^ {0,3}([-+*]|[0-9]{1,9}[.)])(?= |$)", value)
        if not match:
            return None
        rest = value[match.end():]
        padding = len(rest) - len(rest.lstrip(" "))
        # Five or more spaces start code after one marker-padding space.
        padding = padding if 1 <= padding <= 4 and rest.strip() else 1
        return match, match.end() + padding

    def cells(value, single=False):
        # Escaped pipes belong to their cell, including in inline code spans.
        parts, start, escaped = [], 0, False
        for index, char in enumerate(value):
            if char == "|" and not escaped:
                parts.append(value[start:index])
                start = index + 1
            escaped = char == "\\" and not escaped
        if not parts:
            return [value] if single else []
        parts.append(value[start:])
        if not parts[0].strip():
            parts.pop(0)
        if parts and not parts[-1].strip():
            parts.pop()
        return parts

    def table_start(index, value):
        header = cells(value)
        if not header or index + 1 == len(source):
            return False
        following = source[index + 1].expandtabs(4)
        matched, offset = container_content(following)
        following = following[offset:]
        if matched != len(containers) or len(following) - len(following.lstrip(" ")) > 3:
            return False
        delimiter = cells(following, single=True)
        return len(header) == len(delimiter) and all(
            re.fullmatch(r" *:?-+:? *", cell) for cell in delimiter)

    def quote(value):
        return re.match(r"^ {0,3}> ?", value)

    def block_start(value, table=False):
        return (re.match(r"^ {0,3}#{1,6}(?: |$)", value) or
                re.fullmatch(r" {0,3}(?:\*(?: *\*){2,}|-(?: *-){2,}|_(?: *_){2,}) *", value) or
                re.match(r"^ {0,3}(?:<!--|<\?|<![A-Z]|<!\[CDATA\[|(?i:"
                         r"<(?:script|pre|style|textarea)(?= |>|$)|</?(?:"
                         r"address|article|aside|base|basefont|blockquote|body|caption|center|col|"
                         r"colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|"
                         r"form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|"
                         r"menu|menuitem|nav|noframes|ol|optgroup|option|p|param|search|"
                         r"section|source|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul"
                         r")(?= |/?>|$)))", value) or
                (table and re.fullmatch(
                    r" {0,3}(?:</[A-Za-z][A-Za-z0-9-]* *>|"
                    r"<(?!(?i:script|style|pre|textarea)(?=[ />]))[A-Za-z][A-Za-z0-9-]*"
                    r"(?: +[A-Za-z_:][A-Za-z0-9_.:-]*(?: *= *(?:[^\x00\x09-\x0d \"'=<>`]+|"
                    r"'[^'\x00]*'|\"[^\"\x00]*\"))?)* */?>) *", value)))

    def container_content(line):
        offset = 0
        for matched, entry in enumerate(containers):
            value = line[offset:]
            if entry[0] == "quote":
                mark = quote(value)
                if not mark:
                    return matched, offset
                offset += mark.end()
            elif not value.strip(" "):
                if entry[2]:
                    return matched, offset
            elif len(value) - len(value.lstrip(" ")) >= entry[1]:
                offset += entry[1]
            else:
                return matched, offset
        return len(containers), offset

    for index, original in enumerate(source):
        line = original.expandtabs(4)
        matched, container = container_content(line)
        value = line[container:]
        blank = not value.strip(" ")
        leading = len(value) - len(value.lstrip(" "))
        if leaf == "fence" and matched == len(containers):
            closing = re.fullmatch(r" {0,3}" + re.escape(fence[0]) +
                                   "{%d,} *" % len(fence), value)
            if closing:
                leaf, fence = "prose", ""
            yield original, "code"
            continue
        if leaf == "code" and matched == len(containers):
            if blank and next_nonblank <= index:
                next_nonblank = len(source)
                for following_index in range(index + 1, len(source)):
                    following = source[following_index].expandtabs(4)
                    count, offset = container_content(following)
                    if count != len(containers) or following[offset:].strip(" "):
                        next_nonblank = following_index
                        break
            following = line
            if blank:
                following = source[next_nonblank].expandtabs(4) if next_nonblank < len(source) else ""
            count, offset = container_content(following)
            if count == len(containers) and following[offset:].startswith("    "):
                yield original, "code"
                continue
        if blank:
            del containers[matched:]
            paragraph, leaf = False, "prose"
            yield original, "prose"
            continue

        marker = item(value)
        # Only paragraph text may lazily omit enclosing list indentation.
        # A sibling item ends that paragraph regardless of its start number or content.
        interrupts = opener(value) or marker or quote(value) or block_start(value)
        if matched < len(containers) and paragraph and not interrupts:
            yield original, "prose"
            continue
        if matched < len(containers):
            del containers[matched:]
            paragraph, leaf = False, "prose"
        elif leaf in ("fence", "code"):
            paragraph, leaf = False, "prose"

        if leaf == "table" and (index == delimiter_row or
                (leading <= 3 and not opener(value) and not marker and not quote(value) and
                 not block_start(value, table=True))):
            yield original, "table"
            continue
        if leaf == "table":
            leaf = "prose"

        # Setext underlines close a paragraph before list-marker recognition.
        if paragraph and re.fullmatch(r" {0,3}(?:=+|-+) *", value):
            paragraph = False
            yield original, "prose"
            continue
        while marker or quote(value):
            quoted = quote(value)
            if quoted:
                containers.append(["quote", 0, False])
                container += quoted.end()
                value = line[container:]
                paragraph = False
                marker = item(value)
                continue
            match, content = marker
            if block_start(value):
                break
            if paragraph and (not value[content:].strip() or
                              (match[1][0].isdigit() and int(match[1][:-1]) != 1)):
                break
            container += content
            containers.append(["list", content, not value[content:].strip()])
            value = line[container:]
            paragraph = False
            marker = item(value)
        for entry in containers[:-1] if not value.strip() else containers:
            entry[2] = False
        mark = opener(value)
        if mark:
            leaf, fence, paragraph = "fence", mark, False
            kind = "code"
        elif not paragraph and value.startswith("    "):
            leaf, paragraph = "code", False
            kind = "code"
        elif leading <= 3 and table_start(index, value):
            leaf, paragraph, delimiter_row = "table", False, index + 1
            kind = "table"
        else:
            paragraph = bool(value.strip()) and not block_start(value)
            kind = "prose"
        yield original, kind


def _result_structured(line, width, indent=""):
    from dashboard_projection import ELLIPSIS
    # Keep terminal controls inert without clipping retained printable content.
    line = "".join("?" if ch != "\t" and unicodedata.category(ch)[0] == "C" else ch for ch in line)
    prefix, piece, used = indent, [], 0
    for char in line:
        cells = 8 - (display_width(prefix) + used) % 8 if char == "\t" else display_width(char)
        if used + cells > width - display_width(prefix):
            yield prefix + "".join(piece)
            prefix, piece, used = indent + glyph("right", ELLIPSIS[0] == "…") + " ", [], 0
            cells = 8 - display_width(prefix) % 8 if char == "\t" else cells
        piece.append(char)
        used += cells
    yield prefix + "".join(piece)


def render_results(report, width=100, compact=False, heading=None):
    """Scroll ordinary text, preserving the layout of retained code and tables."""
    from room_view import MACHINE, changed_lines, readable, said
    width = max(8, min(200, width))
    lines = [clipped(heading or "OMS / Results", width)]

    def add(text, indent="", limit=None):
        text = str(text)
        for line in wrapped(text, width - len(indent), limit or max(1, len(text)),
                            max_chars=max(400, len(text))):
            lines.append(indent + line)

    def prose(text, indent=""):
        # readable() strips markdown and leading spaces, so structured pieces bypass it.
        ordinary = []

        def flush():
            for line in ordinary:
                text = readable(line, paragraphs=True)
                if text:
                    spaces = line[:len(line) - len(line.lstrip(" \t"))]
                    safe = clipped(spaces + text, max(2000, display_width(spaces + text)))
                    if display_width(indent + safe) <= width:
                        lines.append(indent + safe)
                    else:
                        add(text, indent + clipped(spaces, max(0, width - len(indent) - 1)))
                elif not line.strip():
                    lines.append("")
            ordinary[:] = []

        for line, kind in _result_blocks(text):
            if kind != "prose":
                flush()
                lines.extend(_result_structured(line, width, indent))
            elif not MACHINE.match(line.strip()):
                ordinary.append(line)
        flush()

    rows = listing(report.get("rows"))
    if not rows:
        add("No result in the retained evidence window." if report.get("truncated") else
            "No retained results. Finalize a task to record its owner summary.")
    for number, row in enumerate(rows, 1):
        lines.append("")
        check = mapping(row.get("verification")).get("status")
        outcome = {"accepted": "Accepted by its main", "completed": "Completed; the main recorded a summary",
                   "failed": "Marked failed by its main"}.get(row.get("outcome"), "Not reviewed by its main yet")
        checks = "checks " + str(check).replace("_", " ") if check and check != "not_run" else "no checks run"
        delivery = row.get("delivery")
        if compact:
            add("%s. %s [%s]" % (number, row.get("title") or row.get("task_id") or "Task", outcome), limit=3)
            add("Verify: " + checks, "   ", 2)
            if delivery and delivery != "not_requested":
                add("App: " + delivery, "   ", 2)
            continue
        calls = listing(row.get("calls"))
        latest = mapping(calls[-1]) if calls else {}
        model = row.get("model") or latest.get("served_model") or latest.get("selected_model") or latest.get("requested_model")
        alias = latest.get("alias")
        purpose = PURPOSE_NAMES.get(row.get("purpose")) or TASK_NAMES.get(latest.get("kind")) or PURPOSE_NAMES.get(row.get("role")) or "Work"
        add(" · ".join(([alias] if alias else []) + [MODEL_NAMES.get(model, model) or "unrecorded model", purpose,
                        said(row.get("process_state")), row.get("title") or row.get("task_id") or "Task"]))
        add("Task: " + (row.get("task_id") or "unknown"), limit=3)
        add("Outcome: " + outcome + " · " + checks)
        if row.get("representative_state"):
            add("Stage: %s · Next: %s" % (row["representative_state"], row.get("next_action") or "none"))
        stages = mapping(row.get("stages"))
        if stages:
            add("Stages: " + " / ".join("%s %s" % (name, mapping(stages.get(name)).get("state", "not_run"))
                                        for name in ("implement", "verify", "review", "main_decision")), "  ")
        for line in changed_lines(row):
            add(line)
        if delivery and delivery != "not_requested":
            add("App: %s%s" % (delivery, " / delivery uncertain; inspect before retry" if row.get("delivery_unknown") else ""))
        if row.get("issue"):
            add(row["issue"])
        if row.get("summary"):
            prose(row["summary"], "  ")
        else:
            add("Owner summary pending. Model exit does not establish acceptance.", "  ")
        for call in calls:
            model = call.get("served_model") or call.get("selected_model") or call.get("requested_model")
            line = "%s / %s / exit %s" % (MODEL_NAMES.get(model, model) or "unrecorded model",
                call.get("kind") or "call", call.get("exit", "unknown"))
            if "tokens" in call or "provider_usage" in call:
                token_call = dict(call, attempt_id=call.get("attempt_id") or "results-call")
                line += " / " + call_tokens({"operations": {"recent": [token_call]}}, token_call["attempt_id"],
                                             call.get("state"))
            add(line, "  ", 3)
            if call.get("answer"):
                prose(call["answer"], "    ")
                if call.get("answer_truncated"):
                    add("Answer exceeds the retained preview limit; full source: " +
                        str(call.get("artifact") or "unavailable"), "    ")
            if call.get("artifact"):
                add("Answer artifact: " + call["artifact"], "    ")
        if row.get("artifact"):
            add("Result: " + row["artifact"], limit=4)
        for ref in listing(row.get("evidence")):
            add("Evidence: " + str(ref.get("path", "unavailable")), limit=4)
    lines.append("")
    add(report.get("coverage") or "Retained OMS evidence")
    if report.get("truncated"):
        add("Recent tasks only; select a task ID for its retained answers.")
    return "\n".join(lines)
