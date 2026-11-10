#!/usr/bin/env python3
"""Read-only normalized lifecycle of the active goal, with the evidence for it.

Maps records OMS already keeps onto five states and writes nothing:

  pursuing | paused | achieved | unmet | budget-limited | unknown

Records read: the task packet status and its verification freshness (computed
by agent-task, passed in), the packet's Verification section, and, when a plan
exists, the latest goal-drive terminal row of that plan's runs in
.oms/plan/progress.jsonl. Nothing here is approval or verification evidence of
its own: `achieved` is reported only for a task whose stored Verify command
passed on the current tree (`status: verified` plus fresh fingerprints). An
agent's own claim, a Verification note or an edited status line is `pursuing`.

Usage: goal_lifecycle.py TASK_FILE PLAN_FILE PROGRESS_FILE VERIFICATION
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plan_retro  # noqa: E402

# One table: goal-drive park reason -> normalized state. Any other park reason
# is the driver stopping for the parent (a moved HEAD, a failed landing, ...):
# the goal is neither met nor out of budget, so it is `paused`.
PARK_STATE = {
    "cycles-exhausted": "budget-limited",
    "tasks-exhausted": "budget-limited",
    "replan-budget-used": "budget-limited",
    "stuck": "unmet",
    "acceptance-vacuous": "unmet",
    "acceptance-command-error": "unmet",
    "acceptance-command-changed": "unmet",
}


def metadata(path, key):
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("## "):
                    return ""
                match = re.match(r"^- %s:[ \t]*(.*)$" % re.escape(key), line)
                if match:
                    return match.group(1).strip()
    except OSError:
        pass
    return ""


def last_verify_exit(path):
    """Exit code of the latest recorded verify run; None when none ran."""
    inside, found = False, None
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("## "):
                    inside = line.strip() == "## Verification"
                    continue
                match = re.search(r"\bexit: (\d+)\b", line) if inside else None
                if match:
                    found = int(match.group(1))
    except OSError:
        return None
    return found


def drive_overlay(plan_file, progress_file):
    """Latest unresolved goal-drive park of this plan's runs, else None."""
    plan = plan_retro.read_plan(plan_file)
    if plan is None:
        return None
    rows, _ = plan_retro.read_progress(progress_file)
    ids = {t["id"] for t in plan["tasks"].values() if isinstance(t, dict) and isinstance(t.get("id"), str)}
    runs = {r["run_id"] for r in rows if r.get("kind") in plan_retro.CYCLE_KINDS
            and r.get("task_id") in ids and isinstance(r.get("run_id"), str)}
    last = None
    for index, row in enumerate(rows):
        if row.get("kind") == "terminal" and row.get("run_id") in runs:
            last = (index, row)
    if last is None or last[1].get("status") != "park":
        return None
    # A later passing acceptance resolves the park, as goal-drive itself does.
    if any(r.get("kind") == "acceptance" and r.get("status") == "pass" for r in rows[last[0] + 1:]):
        return None
    return {"run_id": plan_retro.clean(last[1].get("run_id"), 64),
            "reason": plan_retro.clean(last[1].get("reason"), 80) or "unstated",
            "cycle": last[1].get("cycle") if isinstance(last[1].get("cycle"), int) else None}


def lifecycle(task_file, plan_file, progress_file, verification):
    status = metadata(task_file, "status") or "active"
    evidence = {"record": "task-packet", "task_status": status, "verification": verification,
                "last_verify_exit": last_verify_exit(task_file)}
    if status == "verified" and verification == "fresh":
        evidence["rule"] = "verified-fresh"
        return {"state": "achieved", "evidence": evidence}
    if status not in ("active", "verified"):
        evidence["rule"] = "unmapped-status"
        return {"state": "unknown", "evidence": evidence}
    # active, or verified whose pass went stale: not met on the current tree.
    evidence["rule"] = "not-verified-fresh"
    park = drive_overlay(plan_file, progress_file)
    if park:
        evidence.update(record="goal-drive", rule="park:" + park["reason"],
                        park=park, budget_limit=park["reason"] if PARK_STATE.get(park["reason"]) == "budget-limited" else None)
        return {"state": PARK_STATE.get(park["reason"], "paused"), "evidence": evidence}
    return {"state": "pursuing", "evidence": evidence}


if __name__ == "__main__":
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    print(json.dumps(lifecycle(*sys.argv[1:5]), ensure_ascii=False))
