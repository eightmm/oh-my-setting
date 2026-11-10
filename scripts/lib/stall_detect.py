#!/usr/bin/env python3
"""Advisory stall / loop detection for `oms fail-ledger list --stalls`.

A pure function over recorded events plus a small read-only reader for the two
streams OMS already keeps: the failure ledger (.oms/failures.jsonl) and the
goal-drive progress log (.oms/plan/progress.jsonl). Modelled on OpenHands'
StuckDetector, over a recent window of events:

  repeat            same action and same non-error result, N times in a row
  error-repeat      same action and same error, N times in a row
  alternation       two (action, result) pairs taking turns, N events long
  no-new-evidence   goal-drive / autopilot runs that end on the same reason at
                    the same HEAD (nothing new to learn from another cycle)

A finding is an observation. It never kills, retries, parks or approves, and it
is never verification evidence. Stdlib only; reads nothing outside the two
files it is given and writes nothing.

Honest limits: the ledger records failures and the success that resolves one,
not every command, so `repeat` can only fire on a caller-supplied stream.
"""
import argparse
import calendar
import collections
import json
import os
import re
import sys
import time

DEFAULTS = {
    "window": 20,        # OMS_STALL_WINDOW: newest events per stream examined
    "repeat": 4,         # OMS_STALL_REPEAT: same action + same result
    "error_repeat": 3,   # OMS_STALL_ERROR_REPEAT: same action + same error
    "alternate": 6,      # OMS_STALL_ALTERNATE: A/B alternation length
    "no_evidence": 3,    # OMS_STALL_NO_EVIDENCE: cycles ending the same way
    "max_age": 86400,    # OMS_STALL_MAX_AGE seconds; 0 keeps every event
}
ENV = {
    "window": "OMS_STALL_WINDOW",
    "repeat": "OMS_STALL_REPEAT",
    "error_repeat": "OMS_STALL_ERROR_REPEAT",
    "alternate": "OMS_STALL_ALTERNATE",
    "no_evidence": "OMS_STALL_NO_EVIDENCE",
    "max_age": "OMS_STALL_MAX_AGE",
}
NEXT = {
    "repeat": "the same step keeps giving the same result: change the approach "
              "(re-plan, raise effort or tier, oms advise) or ask the human",
    "error-repeat": "the same command keeps failing the same way: do not run it "
                    "unchanged; re-plan, raise effort or tier (oms advise), or ask the human",
    "alternation": "two steps are undoing each other: pick one, re-plan, or ask the human",
    "no-new-evidence": "another cycle would learn nothing new: re-plan, raise effort or "
                       "tier (oms advise), or ask the human before running more cycles",
}

_SUBS = (
    (re.compile(r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?"), "<ts>"),
    (re.compile(r"\b\d{1,2}:\d{2}:\d{2}(?:\.\d+)?\b"), "<time>"),
    (re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b"), "<uuid>"),
    (re.compile(r"\b0x[0-9a-fA-F]+\b"), "<addr>"),
    (re.compile(r"(?:/private)?/(?:tmp|var/folders|var/tmp)/[^\s'\"]+"), "<tmp>"),
    (re.compile(r"\btmp\.[A-Za-z0-9]{4,}\b"), "<tmp>"),
    (re.compile(r"\b(pid|PID|process|thread|port)([ =:#]+)\d+"), r"\1\2<n>"),
    (re.compile(r"\d{4,}"), "<n>"),
    (re.compile(r"\s+"), " "),
)


def normalize_key(text):
    """Collapse cosmetic noise (timestamps, pids, temp paths, long numbers)."""
    text = text if isinstance(text, str) else ""
    for pattern, repl in _SUBS:
        text = pattern.sub(repl, text)
    return text.strip()


def _config(overrides=None):
    cfg = dict(DEFAULTS)
    for key, name in ENV.items():
        raw = os.environ.get(name, "")
        if raw.isdigit():
            cfg[key] = int(raw)
    cfg.update(overrides or {})
    return cfg


def _trailing_run(events):
    """Length of the run of identical (action, result, error) at the end."""
    if not events:
        return 0
    last = events[-1]
    sig = (last["action"], last["result"], last["error"])
    n = 0
    for event in reversed(events):
        if (event["action"], event["result"], event["error"]) != sig:
            break
        n += 1
    return n


def _trailing_alternation(events):
    """Length of the A,B,A,B... suffix (A != B), or 0."""
    pair = [(e["action"], e["result"]) for e in events]
    if len(pair) < 2 or pair[-1] == pair[-2]:
        return 0
    # Each earlier position must equal the one two places after it.
    n = 2
    while n < len(pair) and pair[-1 - n] == pair[-1 - (n - 2)]:
        n += 1
    return n


def detect_stalls(events, config=None):
    """events: dicts {action, result, error, stream, ts?} oldest first.

    Returns findings [{rule, count, key, stream}], newest-stream-state only:
    a loop that has since been broken does not report. `stream` is "cmd" for
    commands or "cycle" for goal-drive / autopilot run endings.
    """
    cfg = _config(config)
    streams = collections.OrderedDict()
    for event in events:
        row = {
            "action": normalize_key(event.get("action")),
            "result": normalize_key(event.get("result")),
            "error": bool(event.get("error")),
            "stream": event.get("stream") or "cmd",
        }
        streams.setdefault(row["stream"], []).append(row)
    findings = []
    for stream, rows in streams.items():
        rows = rows[-cfg["window"]:] if cfg["window"] > 0 else rows
        run = _trailing_run(rows)
        if stream == "cycle":
            limit, rule = cfg["no_evidence"], "no-new-evidence"
        elif rows and rows[-1]["error"]:
            limit, rule = cfg["error_repeat"], "error-repeat"
        else:
            limit, rule = cfg["repeat"], "repeat"
        if limit and run >= limit and rows:
            findings.append({"rule": rule, "count": run, "stream": stream,
                             "key": rows[-1]["action"], "result": rows[-1]["result"]})
            continue
        alt = _trailing_alternation(rows) if stream != "cycle" else 0
        if cfg["alternate"] and alt >= cfg["alternate"]:
            findings.append({"rule": "alternation", "count": alt, "stream": stream,
                             "key": rows[-1]["action"] + " <-> " + rows[-2]["action"],
                             "result": rows[-1]["result"]})
    for finding in findings:
        finding["next"] = NEXT[finding["rule"]]
    return findings


def _epoch(ts):
    try:
        return calendar.timegm(time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return None


def _rows(path):
    """Yield JSON objects from a regular, non-symlink JSONL file; else nothing."""
    if os.path.islink(path) or not os.path.isfile(path):
        return
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict):
                yield row


def ledger_events(path):
    """Failure-ledger rows as events. A resolve row is a pass of that command.

    goal-drive parks are filed here too (kind plan-run); the progress log
    carries the same endings with HEAD, so they are read there instead.
    """
    cmds = {}
    out = []
    for row in _rows(path):
        fp = row.get("fingerprint")
        if row.get("event") == "fail" and row.get("kind") != "plan-run":
            cmds[fp] = str(row.get("cmd") or "")[:400]
            out.append({"action": cmds[fp], "error": True, "stream": "cmd",
                        "result": "exit %s %s" % (row.get("exit"), row.get("summary") or ""),
                        "ts": row.get("ts")})
        elif row.get("event") == "resolved" and fp in cmds:
            out.append({"action": cmds[fp], "result": "ok", "error": False,
                        "stream": "cmd", "ts": row.get("ts")})
    return out


def progress_events(path):
    """goal-drive / autopilot run endings: same reason at the same HEAD."""
    out = []
    for row in _rows(path):
        if row.get("kind") != "terminal":
            continue
        out.append({"action": "%s %s" % (row.get("status"), row.get("reason")),
                    "result": str(row.get("base_sha") or ""), "error": False,
                    "stream": "cycle", "ts": row.get("ts")})
    return out


def _recent(events, max_age):
    if not max_age:
        return events
    cutoff = time.time() - max_age
    return [e for e in events if (_epoch(e.get("ts") or "") or 0) >= cutoff]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ledger", default="")
    parser.add_argument("--progress", default="")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    cfg = _config()
    events = []
    if args.ledger:
        events += ledger_events(args.ledger)
    if args.progress:
        events += progress_events(args.progress)
    findings = detect_stalls(_recent(events, cfg["max_age"]), cfg)
    if args.json:
        print(json.dumps({"schema": 1, "stalls": findings}, ensure_ascii=False))
        return 0
    if not findings:
        print("stalls: none in the last %d recorded event(s) per stream" % cfg["window"])
        return 0
    for finding in findings:
        print("stall: %s x%d  %s" % (finding["rule"], finding["count"], finding["key"][:100]))
        print("  next: %s" % finding["next"])
    print("(advisory: an observation of recorded events, not evidence that work failed or passed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
