"""The board's "work health" line: calls by state, oldest wait, plan progress, open failures, last CI, main context.

Presentation only. Every segment is a count or a reading of a record OMS already keeps; nothing here is verification
evidence, approves anything or changes a call. A segment whose source is not recorded is absent, never a zero.
`collect` runs on the watch loop's existing collection and re-reads a ledger only when its file changed."""
import json
import os
import stat
import subprocess

from dashboard_projection import mapping

CI_TAIL = 64 * 1024
CI_RED = {"failure", "timed_out", "startup_failure"}
FAILED_STATES = {"failed", "timed_out", "abandoned", "orphaned"}
_seen = {}


def _stamp(path):
    try:
        info = os.stat(path)
    except OSError:
        return None
    return (info.st_size, info.st_mtime_ns) if stat.S_ISREG(info.st_mode) else None


def _ci_row(path, branch):
    """The newest ledger row (for `branch` when one is known), read from the file's bounded tail."""
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    except OSError:
        return None
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            return None
        os.lseek(descriptor, max(0, info.st_size - CI_TAIL), os.SEEK_SET)
        raw = os.read(descriptor, CI_TAIL)
    except OSError:
        return None
    finally:
        os.close(descriptor)
    rows = []
    for line in raw.decode("utf-8", "replace").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and isinstance(row.get("status"), str) and isinstance(row.get("ts"), str):
            rows.append(row)
    if branch:
        rows = [row for row in rows if row.get("branch") == branch]
    if not rows:
        return None
    return {"status": rows[-1]["status"], "conclusion": rows[-1].get("conclusion"), "ts": rows[-1]["ts"]}


def _unresolved(repo):
    """Actionable fingerprints in the failure ledger, counted by the verb that owns the retirement rules."""
    try:
        from terminal_panel import ENTRY
        done = subprocess.run(["bash", str(ENTRY), "fail-ledger", "--repo", str(repo), "list", "--unresolved", "--json"],
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=5, check=False)
        rows = json.loads(done.stdout).get("failures") if done.returncode == 0 else None
    except (OSError, ValueError, subprocess.SubprocessError, ImportError, AttributeError):
        return None
    return sum(mapping(row).get("actionable") is True for row in rows) if isinstance(rows, list) else None


def _stalls(root, ledger):
    """Stall findings from the detector library, called in-process; None when neither stream is recorded.

    The parsed events are cached per file stamp; the detector itself is pure and cheap, so it runs every
    collection and a finding ages out on its own (OMS_STALL_MAX_AGE) without any file changing."""
    import stall_detect
    progress = os.path.join(root, ".oms", "plan", "progress.jsonl")
    stamp = (_stamp(ledger), _stamp(progress))
    if stamp == (None, None):
        return None
    cached = _seen.get(("stalls", ledger))
    if cached and cached[0] == stamp:
        events = cached[1]
    else:
        events = []
        if stamp[0] is not None:
            events += stall_detect.ledger_events(ledger)
        if stamp[1] is not None:
            events += stall_detect.progress_events(progress)
        _seen[("stalls", ledger)] = (stamp, events)
    config = stall_detect._config()
    found = stall_detect.detect_stalls(stall_detect._recent(events, config["max_age"]), config)
    return [{"rule": row["rule"], "count": row["count"], "stream": row["stream"], "key": str(row["key"])[:80]}
            for row in found]


def collect(repo, state=None):
    """{"unresolved": int|None, "ci": {...}|None, "stalls": [finding]|None}; fail-open. A ledger is re-read only when it changed."""
    root = os.fspath(repo)
    branch = mapping(mapping(state).get("repo")).get("branch")
    branch = branch if isinstance(branch, str) else ""
    sources = (("ci", os.environ.get("OMS_CI_LEDGER") or os.path.join(root, ".oms", "ci.jsonl"),
                lambda path: _ci_row(path, branch)),
               ("unresolved", os.environ.get("OMS_FAIL_LEDGER") or os.path.join(root, ".oms", "failures.jsonl"),
                lambda path: _unresolved(root)))
    out = {}
    for key, path, read in sources:
        stamp = (_stamp(path), branch)
        cached = _seen.get((key, path))
        if stamp[0] is None:
            out[key] = None
        elif cached and cached[0] == stamp:
            out[key] = cached[1]
        else:
            out[key] = read(path)
            _seen[(key, path)] = (stamp, out[key])
    try:
        out["stalls"] = _stalls(root, sources[1][1])
    except Exception:  # advisory reading: never break the collection
        out["stalls"] = None
    return out


def describe(report, ranked=None, main=None):
    """The same segments as plain text plus structured counts, for `oms panel --health`."""
    from panel_view import attention_items
    ranked = attention_items(report, False) if ranked is None else ranked
    if main is None:
        from room_view import nodes
        main = next((m for m in nodes(report) if m.get("role") == "main"), None)
    found = sorted(segments(report, ranked, False, main), key=lambda part: part[1])
    health = mapping(report.get("work_health"))
    return {"segments": [{"id": key, "text": forms[0]} for unused, unused_order, unused_tone, forms, key in found],
            "stalls": health.get("stalls") if isinstance(health.get("stalls"), list) else []}


def _calls(report):
    from panel_view import LIVE_STATES
    from room_view import call_classifier, nodes
    classify = call_classifier(report)
    counts = {"running": 0, "failed": 0, "done": 0}
    for call in nodes(report):
        if call.get("role") == "main" or call.get("_native") or classify(call) == "past":
            continue
        state = call.get("state")
        key = "running" if state in LIVE_STATES else "failed" if state in FAILED_STATES else "done" if state == "done" else None
        if key:
            counts[key] += 1
    return counts


def segments(report, ranked, unicode, main=None):
    """[(priority, order, tone, forms)] per recorded source; forms run long, medium, short.

    Lower priority numbers survive longer; order is the left-to-right position."""
    from panel_view import age_label
    from room_view import context_note, plan_summary
    found = []

    def add(key, priority, order, tone, uni, asc):
        found.append((priority, order, tone, tuple(uni if unicode else asc), key))

    calls = _calls(report)
    n = calls["running"]
    if n:
        add("running", 4, 0, "running", ("▶ running %d" % n, "▶ run %d" % n, "▶%d" % n), ("> running %d" % n, "> run %d" % n, "run %d" % n))
    needs = [item for item in ranked if item.get("cls") == "needs"]
    if needs:
        n = len(needs)
        stamps = sorted(str(item["stamp"]) for item in needs if item.get("stamp"))
        age = age_label(stamps[0]) if stamps else ""
        add("waiting", 1, 1, "alert",
            ("⏸ waiting on you %d" % n + (" · oldest " + age if age else ""), "⏸ waiting %d" % n + (" · " + age if age else ""),
             "⏸%d" % n + (" " + age if age else "")),
            ("! waiting on you %d" % n + (" / oldest " + age if age else ""), "! waiting %d" % n + (" / " + age if age else ""),
             "you %d" % n + (" " + age if age else "")))
    stalled = mapping(report.get("work_health")).get("stalls")
    n = len(stalled) if isinstance(stalled, list) else 0
    if n:
        add("stalls", 2, 2, "alert", ("↻ stalls %d" % n, "↻ stalls %d" % n, "↻%d" % n),
            ("! stalls %d" % n, "! stalls %d" % n, "stall %d" % n))
    n = calls["failed"]
    if n:
        add("failed", 3, 3, "bad", ("✗ failed %d" % n, "✗ fail %d" % n, "✗%d" % n), ("x failed %d" % n, "x fail %d" % n, "fail %d" % n))
    n = calls["done"]
    if n:
        add("done", 9, 4, "good", ("✓ done %d" % n, "✓ done %d" % n, "✓%d" % n), ("+ done %d" % n, "+ done %d" % n, "done %d" % n))
    summary = plan_summary(report)
    if summary and summary[2]:
        form = ("plan %d/%d tasks" % summary[1:3], "plan %d/%d" % summary[1:3], "plan %d/%d" % summary[1:3])
        add("plan", 7, 5, "dim", form, form)
    health = mapping(report.get("work_health"))
    open_failures = health.get("unresolved")
    if type(open_failures) is int and open_failures > 0:
        form = ("! %d unresolved" % open_failures, "! %d unresolved" % open_failures, "! %d open" % open_failures)
        add("unresolved", 6, 6, "bad", form, form)
    ci = mapping(health.get("ci"))
    if ci.get("status"):
        age = age_label(ci.get("ts"))
        conclusion = str(ci.get("conclusion") or "unknown")[:12]
        if ci["status"] != "completed":
            tone, word, marks = "running", "running", ("⟳", "~")
        elif ci.get("conclusion") == "success":
            tone, word, marks = "good", "passed", ("✓", "+")
        elif ci.get("conclusion") in CI_RED:
            tone, word, marks = "bad", "failed", ("✗", "x")
        else:
            tone, word, marks = "dim", conclusion, ("·", "-")

        def forms(mark):
            return ("CI %s %s%s" % (mark, word, " " + age + " ago" if age and word != "running" else ""),
                    "CI %s%s" % (mark, " " + age if age and word != "running" else " " + word),
                    "CI%s%s" % (mark, age or ""))
        add("ci", 5, 7, tone, forms(marks[0]), forms(marks[1]))
    reading = context_note(report, main) if main is not None and report.get("capacity_stale") is not True else None
    if reading:
        hot = "! " if reading[0] >= 85 else ""
        form = (hot + "ctx %d%% used" % reading[0], hot + "ctx %d%%" % reading[0], hot + "ctx %d%%" % reading[0])
        add("context", 8, 8, "alert" if hot else "dim", form, form)
    return found


def lines(report, ranked, width, height, unicode, main=None):
    """Rows of (text, spans) for the board head: none, one, or two at 100+ columns; never wider than `width`.

    Medium forms from 50 columns, short forms below. Whole segments drop, lowest priority first."""
    if height < 16 or width < 12:
        return []
    from panel_view import display_width
    tier = 0 if width >= 80 else 1 if width >= 50 else 2
    sep = (" · " if unicode else " / ") if tier < 2 else " "
    found = [(prio, order, tone, forms[tier]) for prio, order, tone, forms, unused_key in segments(report, ranked, unicode, main)]
    if not found:
        return []

    def fit(parts):
        parts = sorted(parts, key=lambda part: part[1])
        while parts and display_width(sep.join(part[3] for part in parts)) > width:
            parts.pop(max(range(len(parts)), key=lambda i: parts[i][0]))
        return parts

    groups = [fit(found)]
    if width >= 100 and height >= 24 and len(groups[0]) < len(found):
        # Two lines when one cannot hold everything: work state first, then plan, failures, CI and context.
        groups = [part for part in (fit([p for p in found if p[1] <= 4]), fit([p for p in found if p[1] > 4])) if part]
    rows = []
    for parts in groups:
        spans = []
        for index, (unused, unused_order, tone, text) in enumerate(parts):
            if index:
                spans.append((sep, "dim"))
            spans.append((text, tone))
        rows.append(("".join(text for text, unused in spans), spans))
    return rows
