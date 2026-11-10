#!/usr/bin/env python3
"""Advisory "should this be delegated" verdict for a delegation brief.

    delegate_advice.py --brief FILE|- --access write|read [--verify yes|no]
        [--allowed a,b] [--plan FILE] [--self-id ID] [--json]

Facts only, no model call, nothing written: brief size, the write paths it names
(brief text plus plan allowed paths), whether another claimed/running/review/
landing plan task declares an overlapping path, and whether a mechanical check
exists. The verdict is one of inline, researcher, worker. It is advice for the
parent and never refuses, delays or routes a delegation; a verdict is not
verification evidence. Exit 0 always.
"""

import argparse
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import path_scope  # noqa: E402

DEFAULT_SMALL_BYTES = 600
DEFAULT_SMALL_PATHS = 3
ACTIVE_STATES = ("claimed", "running", "review", "landing")
MAX_BYTES = 262144
PATH = re.compile(r"(?<![\w/.-])(?:[\w.-]+/)+[\w.-]+|(?<![\w/.-])[\w-]+\.(?:py|sh|md|json|jsonl|toml|ya?ml|txt|"
                  r"ts|js|c|h|go|rs|cfg|ini)\b")


def _int_env(name, default):
    try:
        value = int(os.environ.get(name, ""))
    except ValueError:
        return default
    return value if value > 0 else default


def thresholds():
    return {"small_bytes": _int_env("OMS_DELEGATE_ADVICE_SMALL_BYTES", DEFAULT_SMALL_BYTES),
            "small_paths": _int_env("OMS_DELEGATE_ADVICE_SMALL_PATHS", DEFAULT_SMALL_PATHS)}


def brief_paths(text, allowed):
    found = []
    for item in list(allowed) + [m.group(0).rstrip(".") for m in PATH.finditer(text)]:
        item = item.strip().lstrip("./")
        if item and item not in found:
            found.append(item)
    return found


def _overlap(a, b):
    if a == b or a.startswith(b.rstrip("/") + "/") or b.startswith(a.rstrip("/") + "/"):
        return True
    try:
        return path_scope.matches(a, b) or path_scope.matches(b, a)
    except (ValueError, TypeError):
        return False


def active_overlaps(plan_file, self_id, paths):
    try:
        with open(plan_file, encoding="utf-8") as fh:
            tasks = json.load(fh).get("tasks", {})
    except (OSError, ValueError, AttributeError):
        return []
    found = []
    for tid, task in sorted(tasks.items()) if isinstance(tasks, dict) else []:
        if tid == self_id or not isinstance(task, dict) or task.get("state") not in ACTIVE_STATES:
            continue
        for theirs in task.get("allowed_paths") or []:
            hit = next((p for p in paths if isinstance(theirs, str) and _overlap(p, theirs)), None)
            if hit:
                found.append({"task": tid, "state": task["state"], "path": theirs})
                break
    return found


def advise(text, access, has_check, allowed=(), plan_file="", self_id=""):
    limits = thresholds()
    size = len(text.encode("utf-8", "replace"))
    paths = brief_paths(text, allowed)
    overlaps = active_overlaps(plan_file, self_id, paths) if plan_file and access == "write" else []
    reasons = []
    if access == "read":
        verdict = "researcher"
        reasons.append("read-only access: exploration or audit whose notes stay out of the main context")
    elif overlaps:
        verdict = "inline"
        reasons.append("overlaps active task %s (%s): not parallel-safe, do it after that lease settles"
                       % (overlaps[0]["task"], overlaps[0]["path"]))
    elif size < limits["small_bytes"] and len(paths) < limits["small_paths"]:
        verdict = "inline"
        reasons.append("small sequential edit: brief %d bytes < %d and %d paths < %d; setup costs more than the edit"
                       % (size, limits["small_bytes"], len(paths), limits["small_paths"]))
    elif paths and has_check:
        verdict = "worker"
        reasons.append("bounded write: brief names %d path(s), no active task touches them, and a mechanical check exists"
                       % len(paths))
    else:
        verdict = "researcher"
        if not paths:
            reasons.append("no write paths named: scope is open, so investigate read-only (--read-only) first")
        if not has_check:
            reasons.append("no mechanical check: a patch could only be judged by hand, so keep edits with the parent")
    return {"advisory": True, "verdict": verdict, "reasons": reasons,
            "facts": {"brief_bytes": size, "access": access, "write_paths": len(paths), "paths": paths[:20],
                      "has_check": bool(has_check), "active_overlaps": overlaps},
            "thresholds": limits}


def render(result):
    lines = ["advice: %s (advisory; a delegate is never refused)" % result["verdict"]]
    lines += ["  - " + r for r in result["reasons"]]
    return "\n".join(lines)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--brief", required=True)
    ap.add_argument("--access", choices=("write", "read"), default="write")
    ap.add_argument("--verify", choices=("yes", "no"), default="no")
    ap.add_argument("--allowed", default="")
    ap.add_argument("--plan", default="")
    ap.add_argument("--self-id", default="")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    if args.brief == "-":
        text = sys.stdin.read(MAX_BYTES)
    else:
        with open(args.brief, encoding="utf-8", errors="replace") as fh:
            text = fh.read(MAX_BYTES)
    allowed = [a for a in re.split(r"[,\s]+", args.allowed) if a]
    result = advise(text, args.access, args.verify == "yes", allowed, args.plan, args.self_id)
    print(json.dumps(result, sort_keys=True) if args.json else render(result))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
