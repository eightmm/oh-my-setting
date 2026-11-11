"""Read-only side-by-side comparison of the attempts recorded for one task (`oms panel --compare TASK`).

One column per write attempt, in creation order. Nothing here ranks, scores or
recommends an attempt, and nothing accepts, verifies or lands one: acceptance
stays on the existing admission path. A field with no recorded evidence reads
`unrecorded`; absence is never reported as a failure.
"""

import datetime
from collections import Counter

import panel_results as results
from dashboard_projection import clean
from panel_view import clipped, display_width, fixed_column, wrapped

LABEL = 11
GAP = 2
WIDE = 100
MIN_COLUMN = 16
LIST_CAP = 4
JSON_LIST_CAP = 50
TERMINAL = {"done", "failed", "cancelled", "timed_out", "abandoned"}
UNRECORDED = "unrecorded"
NOTE = ("Order is creation time, not a ranking. Read-only: this view neither verifies nor accepts "
        "an attempt; use the existing admission path.")


def _int(value):
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _stamp(value):
    try:
        parsed = datetime.datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    except ValueError:
        return None
    return (parsed if parsed.tzinfo else parsed.replace(tzinfo=datetime.timezone.utc)).timestamp()


def _verify(delegates):
    """Latest delegate row that carries a verifier exit; no such row means no receipt."""
    for row in reversed(delegates):
        code = _int(row.get("verify_exit"))
        if code is not None:
            return {"status": "passed" if code == 0 else "failed", "exit": code}
    return {"status": UNRECORDED, "exit": None}


def _review(rows, digest, task_id):
    if not digest:
        return {"status": UNRECORDED, "seats": {}, "exit": None, "gate_verify_exit": None}
    found = [r for r in rows if r.get("kind") == "review-outcome" and isinstance(r.get("review"), dict)
             and isinstance(r["review"].get("candidate"), dict)
             and r["review"]["candidate"].get("patch_sha256") == digest
             and r.get("task_id") in (task_id, None, "")]
    if not found:
        return {"status": UNRECORDED, "seats": {}, "exit": None, "gate_verify_exit": None}
    latest = found[-1]
    seats = latest["review"].get("seats")
    counts = Counter(clean(s.get("verdict"), 16) or UNRECORDED for s in seats if isinstance(s, dict)) \
        if isinstance(seats, list) else Counter()
    return {"status": "recorded", "seats": dict(counts), "exit": _int(latest.get("exit")),
            "gate_verify_exit": _int(latest["review"].get("gate_verify_exit"))}


def _duration(attempt):
    usage, reports = attempt.get("usage") or {}, attempt.get("usage_reports") or {}
    reported = _int(usage.get("duration_ms"))
    if reported and _int(reports.get("duration_ms")):
        return reported // 1000
    start, end = _stamp(attempt.get("created_at")), _stamp(attempt.get("updated_at"))
    if attempt.get("state") in TERMINAL and start is not None and end is not None and end >= start:
        return int(end - start)
    return None


def _tokens(attempt):
    usage, reports = attempt.get("usage") or {}, attempt.get("usage_reports") or {}
    value = _int(usage.get("tokens"))
    return value if value and _int(reports.get("tokens")) else None


def collect(repo, task_id, _records_data=None):
    """Compare the write attempts of one task from retained lifecycle and artifact evidence."""
    if not isinstance(task_id, str) or not results.ID.fullmatch(task_id):
        raise ValueError("invalid task ID")
    attempts, rows = _records_data if _records_data is not None else \
        results._records(repo, full=True, task_id=task_id)
    delegates = {}
    for row in rows:
        if row.get("kind") == "delegate" and isinstance(row.get("attempt_id"), str):
            delegates.setdefault(row["attempt_id"], []).append(row)

    def patch_row(attempt_id):
        return next((r for r in reversed(delegates.get(attempt_id, [])) if r.get("patch") and r.get("patch_sha256")), None)

    chosen = [a for a in attempts if a.get("task_id") == task_id
              and (a.get("refs", {}).get("panel_access") == "write" or patch_row(a.get("attempt_id")))]
    chosen.sort(key=lambda a: a.get("created_at") or "")
    if not chosen:
        raise ValueError("no write attempts are recorded for task %s" % task_id)
    entries, path_sets = [], {}
    for number, attempt in enumerate(chosen, 1):
        ident = attempt.get("attempt_id")
        refs = attempt.get("refs", {})
        patch = patch_row(ident)
        changes = None
        if patch:
            try:
                paths = list(dict.fromkeys(results._patch_paths(results.verified_patch(repo, patch))))
                stat = results._diffstat(repo, patch)
                changes = {"files": stat["count"], "added": stat["added"], "removed": stat["removed"]}
                path_sets[ident] = paths
            except (OSError, ValueError, TypeError):
                changes = {"issue": "patch evidence unavailable"}
        entries.append({
            "index": number, "attempt_id": clean(ident, 80),
            "provider": clean(refs.get("panel_provider") or attempt.get("provider"), 60) or UNRECORDED,
            "model": clean(refs.get("panel_model"), 80) or UNRECORDED,
            "effort": clean(refs.get("panel_effort"), 20) or UNRECORDED,
            "state": clean(attempt.get("state"), 40) or UNRECORDED,
            "verify": _verify(delegates.get(ident, [])),
            "review": _review(rows, patch.get("patch_sha256") if patch else None, task_id),
            "changes": changes, "only_here": None, "shared": None,
            "tokens": _tokens(attempt), "duration_s": _duration(attempt)})
    for entry, attempt in zip(entries, chosen):
        mine = path_sets.get(attempt.get("attempt_id"))
        if mine is None:
            continue
        others = set().union(*(p for i, p in path_sets.items() if i != attempt.get("attempt_id")))
        entry["only_here"] = [clean(p, 160) for p in mine if p not in others][:JSON_LIST_CAP]
        entry["shared"] = [clean(p, 160) for p in mine if p in others][:JSON_LIST_CAP]
        entry["only_count"] = sum(1 for p in mine if p not in others)
        entry["shared_count"] = sum(1 for p in mine if p in others)
    notes = [NOTE]
    if len(entries) < 2:
        notes.append("Only one attempt is recorded for this task; nothing to compare.")
    if len(path_sets) < len([e for e in entries if e["changes"] is not None]):
        notes.append("Only-here and shared files compare attempts with a readable patch.")
    return {"schema": 1, "kind": "oms-panel-compare", "task_id": task_id, "ranked": False,
            "order": "creation time", "attempts": entries, "notes": notes}


def _seconds(value):
    if value is None:
        return UNRECORDED
    hours, rest = divmod(value, 3600)
    minutes, seconds = divmod(rest, 60)
    return "%dh%02dm" % (hours, minutes) if hours else "%dm%02ds" % (minutes, seconds) if minutes else "%ds" % seconds


def _amount(value):
    if value is None:
        return UNRECORDED
    return "%gM" % (value / 1e6) if value >= 1000000 else "%gk" % (value / 1000.0) if value >= 1000 else str(value)


def _file_lines(entry, key):
    names = entry.get(key)
    if names is None:
        return ["n/a" if entry["changes"] is None else UNRECORDED]
    total = entry.get(key.replace("_here", "") + "_count", len(names)) if key == "only_here" else entry.get("shared_count", len(names))
    lines = ["%d file%s" % (total, "" if total == 1 else "s")] + names[:LIST_CAP]
    return lines + (["+%d more" % (total - LIST_CAP)] if total > LIST_CAP else [])


def _cells(entry):
    verify, review, changes = entry["verify"], entry["review"], entry["changes"]
    check = verify["status"] if verify["exit"] is None else "%s (exit %d)" % (verify["status"], verify["exit"])
    if review["status"] == UNRECORDED:
        verdicts = [UNRECORDED]
    else:
        seats = " ".join("%d %s" % (n, v) for v, n in sorted(review["seats"].items())) or "no seats"
        verdicts = [seats] + (["gate verify exit %d" % review["gate_verify_exit"]]
                              if review["gate_verify_exit"] is not None else [])
    stat = ("no patch" if changes is None else changes.get("issue") or
            "%d file%s +%d -%d" % (changes["files"], "" if changes["files"] == 1 else "s", changes["added"], changes["removed"]))
    return [("provider", [entry["provider"]]), ("model", [entry["model"]]), ("effort", [entry["effort"]]),
            ("state", [entry["state"]]), ("verify", [check]), ("review", verdicts), ("diff", [stat]),
            ("only here", _file_lines(entry, "only_here")), ("shared", _file_lines(entry, "shared")),
            ("tokens", [_amount(entry["tokens"])]), ("duration", [_seconds(entry["duration_s"])])]


def render(report, width=100):
    """Columns side by side from 100 cells wide, one block per attempt below that."""
    width = max(20, min(200, width))
    entries = report["attempts"]
    lines = [clipped("OMS / Compare task %s (%d attempt%s)" % (
        report["task_id"], len(entries), "" if len(entries) == 1 else "s"), width), ""]
    cells = [dict(_cells(entry)) for entry in entries]
    labels = [name for name, _ in _cells(entries[0])]
    column = (width - LABEL - GAP * len(entries)) // len(entries)
    if width >= WIDE and column >= MIN_COLUMN:
        def line(label, values):
            return (fixed_column(label, LABEL) + "".join(" " * GAP + fixed_column(v, column) for v in values)).rstrip()
        lines.append(line("", ["#%d %s" % (e["index"], e["attempt_id"]) for e in entries]))
        lines.append(line("", ["-" * min(column, 8 + len(e["attempt_id"])) for e in entries]))
        for label in labels:
            height = max(len(cell[label]) for cell in cells)
            for row in range(height):
                lines.append(line(label if row == 0 else "", [cell[label][row] if row < len(cell[label]) else "" for cell in cells]))
    else:
        for entry, cell in zip(entries, cells):
            lines.append(clipped("#%d %s" % (entry["index"], entry["attempt_id"]), width))
            for label in labels:
                for row, value in enumerate(cell[label]):
                    lines.append(clipped("  " + fixed_column(label if row == 0 else "", LABEL) + value, width))
            lines.append("")
        lines.pop()
    for note in report["notes"]:
        lines.append("")
        lines.extend(wrapped(note, width, 6))
    return "\n".join(lines)
