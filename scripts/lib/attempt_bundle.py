"""Read-only post-mortem bundle for one lifecycle attempt.

Gathers what OMS already recorded for an attempt (lifecycle events, artifact-index rows that carry its
attempt id or answer one of them, fail-ledger rows from the attempt's time window, and the tool-call
timeline of a provider transcript whose path OMS already knows) as short facts with the file each came
from. Nothing is copied wholesale: no commands, prompts, answers, tool arguments or tool output. Nothing
is written, retried or judged; observation is never verification or approval evidence.

Every printed string passes the shared secret scanner first; a scan that cannot run prints nothing."""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

LIB = os.path.dirname(os.path.abspath(__file__))
if LIB not in sys.path:
    sys.path.insert(0, LIB)

from oms_runtime.common import CoreError, read_jsonl  # noqa: E402
from oms_runtime.context_inspect import InspectionError  # noqa: E402
from oms_runtime import context_session  # noqa: E402

MAX_EVENTS = 200
MAX_ROWS = 50
MAX_FAILURES = 50
TIMELINE_STEPS = 50
SAFE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,159}\Z")
HEADER = ("Observation only: recorded facts and pointers, no commands, prompts, answers or tool output. "
          "Never verification, approval or a statement of cause.")
# Short facts of an artifact-index row; each is a label, count, digest or id, never text from the work.
ROW_FIELDS = ("event_id", "ts", "kind", "provider", "exit", "verify_exit", "task_id", "run_id", "role",
              "model_class", "requested_model", "selected_model", "fallback_model", "fallback_used",
              "fallback_reason", "reasoning_effort", "answer_quality", "prompt_sha256", "prompt_bytes",
              "parent_event_id", "artifact", "patch", "patch_sha256", "tokens", "duration_s", "status", "covers")


def _ts(value):
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()
    except (TypeError, ValueError):
        return None


def _short(value, limit=200):
    if isinstance(value, bool) or value is None or isinstance(value, (int, float)):
        return value
    if isinstance(value, str):
        return value[:limit]
    if isinstance(value, list):
        return [_short(item, 80) for item in value[:16]]
    return None


def lifecycle_section(rows, attempt):
    mine = [row for row in rows if row.get("attempt_id") == attempt["attempt_id"]]
    events, gap, previous = [], 0.0, None
    for row in mine[:MAX_EVENTS]:
        stamp = _ts(row.get("ts"))
        if stamp is not None and previous is not None:
            gap = max(gap, stamp - previous)
        previous = stamp if stamp is not None else previous
        item = {key: row[key] for key in ("seq", "ts", "event_type", "from_state", "to_state", "reason_code",
                                          "event_id") if row.get(key) not in (None, "")}
        if isinstance(row.get("refs"), dict) and row["refs"]:
            item["ref_names"] = sorted(str(key)[:60] for key in row["refs"])[:16]
        events.append(item)
    return {"source": ".oms/lifecycle/events.jsonl", "state": attempt["state"], "terminal": attempt["terminal"],
            "reason_code": attempt.get("reason_code") or None, "created_at": attempt["created_at"],
            "updated_at": attempt["updated_at"], "events_total": len(mine), "events": events,
            "events_omitted": max(0, len(mine) - MAX_EVENTS), "longest_gap_s": int(gap),
            "usage": attempt.get("usage"), "ref_names": sorted(attempt.get("refs", {}))[:32]}


def _review(review):
    if not isinstance(review, dict):
        return None
    seats = review.get("seats")
    out = {"seats": len(seats) if isinstance(seats, list) else None}
    for key in ("verdict", "outcome", "status", "decision"):
        if isinstance(review.get(key), str):
            out[key] = review[key][:40]
    if isinstance(seats, list):
        out["seat_verdicts"] = [str(seat.get(key))[:40] for seat in seats[:8] if isinstance(seat, dict)
                                for key in ("verdict", "outcome", "status") if seat.get(key) is not None][:16]
    return out


def artifact_section(repo, attempt_id):
    source = ".oms/artifacts/index.jsonl"
    try:
        rows = read_jsonl(Path(repo) / source)
    except CoreError:
        return {"source": source, "readable": False, "rows": []}
    own = [row for row in rows if row.get("attempt_id") == attempt_id]
    ids = {row.get("event_id") for row in own if row.get("event_id")}
    follow = [row for row in rows if row.get("attempt_id") != attempt_id and row.get("parent_event_id") in ids]
    shown = []
    for how, group in (("attempt_id", own), ("answers parent_event_id", follow)):
        for row in group:
            item = {key: _short(row[key]) for key in ROW_FIELDS if key in row and _short(row[key]) is not None}
            item["linked_by"] = how
            if row.get("result_lint"):
                item["result_lint"] = _short(row["result_lint"])
            review = _review(row.get("review"))
            if review is not None:
                item["review"] = review
            shown.append(item)
    shown.sort(key=lambda item: str(item.get("ts", "")))
    return {"source": source, "readable": True, "rows_total": len(shown), "rows": shown[:MAX_ROWS],
            "rows_omitted": max(0, len(shown) - MAX_ROWS)}


def failure_section(repo, attempt):
    source = ".oms/failures.jsonl"
    path = os.environ.get("OMS_FAIL_LEDGER") or str(Path(repo) / source)
    first, last = _ts(attempt["created_at"]), _ts(attempt["updated_at"])
    try:
        rows = read_jsonl(Path(path))
    except CoreError:
        return {"source": source, "readable": False, "rows": []}
    found = []
    for number, row in enumerate(rows, 1):
        stamp = _ts(row.get("ts"))
        if stamp is None or first is None or last is None or not first <= stamp <= last:
            continue
        item = {"line": number}
        for key in ("ts", "fingerprint", "kind", "event", "exit", "agent"):
            if key in row and _short(row[key], 80) is not None:
                item[key] = _short(row[key], 80)
        found.append(item)
    return {"source": source, "readable": True,
            "match": "rows stamped inside the attempt's time window; the ledger records no attempt id, "
                     "so these may belong to other work",
            "rows_total": len(found), "rows": found[:MAX_FAILURES], "rows_omitted": max(0, len(found) - MAX_FAILURES)}


def transcript_pointer(repo, attempt, explicit):
    """The transcript path OMS recorded for this attempt's panel participant, or the explicit one."""
    if explicit:
        return explicit, "--transcript"
    participant = attempt.get("refs", {}).get("panel_room_participant") or attempt["attempt_id"]
    if not isinstance(participant, str) or not SAFE.fullmatch(participant):
        return None, None
    relative = ".oms/hooks/panel-activity/%s.json" % participant
    try:
        row = json.loads(Path(repo, relative).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None, None
    if (isinstance(row, dict) and row.get("attempt") == attempt["attempt_id"]
            and isinstance(row.get("transcript_path"), str) and row["transcript_path"]):
        return row["transcript_path"], relative
    return None, None


def build_bundle(repo, attempt, event_rows, *, transcript=None):
    path, origin = transcript_pointer(repo, attempt, transcript)
    section = {"source": origin, "available": False}
    if path:
        try:
            section = dict(context_session.inspect_timeline(str(repo), path, rows=TIMELINE_STEPS),
                           source=origin, available=True)
        except InspectionError as exc:
            section = {"source": origin, "available": False, "reason": str(exc)[:160]}
    else:
        section["reason"] = "no transcript path recorded for this attempt; pass --transcript PATH"
    return {"schema": 1, "kind": "oms-attempt-bundle", "attempt_id": attempt["attempt_id"],
            "run_id": attempt.get("run_id") or None, "task_id": attempt.get("task_id") or None,
            "provider": attempt.get("provider") or None, "tool": attempt.get("tool") or None,
            "notice": HEADER,
            "lifecycle": lifecycle_section(event_rows, attempt),
            "artifact_rows": artifact_section(repo, attempt["attempt_id"]),
            "fail_ledger": failure_section(repo, attempt),
            "transcript_timeline": section}


def _leaves(value, out):
    if isinstance(value, str):
        out.append(value)
    elif isinstance(value, dict):
        for item in value.values():
            _leaves(item, out)
    elif isinstance(value, list):
        for item in value:
            _leaves(item, out)


SCAN_SCRIPT = ('. "$1/agent-memory-common.sh"; shift; for f in "$@"; do rc=0; '
               'agent_memory_file_secret_status "$f" || rc=$?; printf "%s\\n" "$rc"; done')


def _scan(texts):
    """True per text when the shared scanner flags it; None when the scan cannot run."""
    try:
        with tempfile.TemporaryDirectory(prefix="oms-bundle-scan.") as scratch:
            paths = []
            for index, text in enumerate(texts):
                path = os.path.join(scratch, "t%d" % index)
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(text + "\n")
                paths.append(path)
            done = subprocess.run(["bash", "-c", SCAN_SCRIPT, "scan", LIB, *paths], capture_output=True,
                                  text=True, timeout=60, check=False, stdin=subprocess.DEVNULL)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    verdicts = done.stdout.split()
    if done.returncode != 0 or len(verdicts) != len(texts) or any(v not in ("0", "1") for v in verdicts):
        return None
    # 0 is a hit, 1 is clean; anything else was a failed scan.
    return [verdict == "0" for verdict in verdicts]


def _withhold(value, flagged):
    if isinstance(value, str):
        return "[withheld: secret-like]" if value in flagged else value
    if isinstance(value, dict):
        return {key: _withhold(item, flagged) for key, item in value.items()}
    if isinstance(value, list):
        return [_withhold(item, flagged) for item in value]
    return value


def scanned(bundle):
    """The bundle with flagged strings withheld; raises CoreError when the scan cannot run."""
    whole = json.dumps(bundle, sort_keys=True, ensure_ascii=False)
    verdict = _scan([whole])
    if verdict is None:
        raise CoreError("secret scan unavailable; nothing printed")
    if not verdict[0]:
        return bundle
    leaves = []
    _leaves(bundle, leaves)
    unique = [text for text in dict.fromkeys(leaves) if text]
    verdicts = _scan(unique)
    if verdicts is None:
        raise CoreError("secret scan unavailable; nothing printed")
    return _withhold(bundle, {text for text, hit in zip(unique, verdicts) if hit})


def _line(item, keys):
    return " ".join("%s=%s" % (key, item[key]) for key in keys if item.get(key) is not None)


def render_text(bundle):
    out = ["Attempt post-mortem bundle: %s" % bundle["attempt_id"], bundle["notice"],
           "provider/tool: %s/%s  run/task: %s/%s" % (bundle["provider"] or "-", bundle["tool"] or "-",
                                                     bundle["run_id"] or "-", bundle["task_id"] or "-"), ""]
    life = bundle["lifecycle"]
    out.append("Lifecycle [%s]: state=%s terminal=%s reason=%s events=%d longest_gap_s=%d" % (
        life["source"], life["state"], life["terminal"], life["reason_code"] or "-", life["events_total"],
        life["longest_gap_s"]))
    for event in life["events"]:
        out.append("  #%s %s %s %s->%s %s" % (event.get("seq"), event.get("ts"), event.get("event_type"),
                                              event.get("from_state", "-"), event.get("to_state", "-"),
                                              event.get("reason_code", "")))
    art = bundle["artifact_rows"]
    out.extend(["", "Artifact-index rows [%s]: %s" % (art["source"], art.get("rows_total", "unreadable"))])
    for row in art["rows"]:
        out.append("  " + _line(row, ("event_id", "ts", "kind", "provider", "exit", "verify_exit", "linked_by",
                                      "role", "selected_model", "model_class", "fallback_used", "answer_quality",
                                      "prompt_sha256", "status")))
        if row.get("result_lint"):
            out.append("    result_lint: " + ",".join(str(c) for c in row["result_lint"]))
        if row.get("review"):
            out.append("    review: " + json.dumps(row["review"], sort_keys=True))
    fail = bundle["fail_ledger"]
    out.extend(["", "Fail-ledger rows [%s]: %s (%s)" % (fail["source"], fail.get("rows_total", "unreadable"),
                                                        fail.get("match", ""))])
    for row in fail["rows"]:
        out.append("  " + _line(row, ("line", "ts", "kind", "event", "exit", "fingerprint")))
    line = bundle["transcript_timeline"]
    out.append("")
    if not line.get("available"):
        out.append("Transcript timeline: not available (%s)" % line.get("reason", "not read"))
    else:
        first = line["first_error"]
        out.append("Transcript timeline [%s]: steps=%d errors=%d pending=%d first_error_step=%s steps_after=%s" % (
            line["source"], line["steps_total"], line["errors"], line["pending"],
            first["step"] if first else "-", line["steps_after_first_error"]))
        for step in line["steps"]:
            mark = " FIRST ERROR" if first and step["step"] == first["step"] else ""
            out.append("  %d %s %s duration_ms=%s arg_bytes=%s result_bytes=%s%s%s" % (
                step["step"], step["tool"], "error" if step["error"] else "ok" if step["ok"] else "no-result",
                step["duration_ms"], step["argument_bytes"], step["result_bytes"],
                " repeat_of=%d" % step["repeat_of"] if step["repeat_of"] else "", mark))
        if line["steps_omitted"]:
            out.append("  (%d more steps; use context-inspect --session PATH --timeline)" % line["steps_omitted"])
    return "\n".join(out)
