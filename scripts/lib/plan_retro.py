#!/usr/bin/env python3
"""Read-only retrospective of the current plan: what took more than one try.

Counts only what the plan and its progress ledger already recorded: task
states, repairs, amendments, blocked and cancelled tasks with their recorded
reasons, cycles spent per task, acceptance runs and where a drive parked.
Nothing is written, no model is asked and nothing here is approval or
verification evidence; turning a line into a Decision or a pin is the
parent's own act.

Usage: plan_retro.py PLAN_FILE [--json]
"""
import json
import os
import sys

MAX_READ = 8 * 1024 * 1024
MAX_ROWS = 20
TEXT = 160
CYCLE_KINDS = ("goal-cycle", "autopilot-cycle")


def clean(value, limit=TEXT):
    text = "".join(ch if ch.isprintable() else " " for ch in str(value or ""))
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit - 1] + "…"


def read_plan(path):
    try:
        if os.path.islink(path) or os.path.getsize(path) > MAX_READ:
            return None
        with open(path, encoding="utf-8") as handle:
            plan = json.load(handle)
    except (OSError, ValueError):
        return None
    return plan if isinstance(plan, dict) and isinstance(plan.get("tasks"), dict) else None


def read_progress(path):
    """(rows, complete): the ledger's tail when it is longer than the read cap."""
    rows, complete = [], True
    try:
        if os.path.islink(path):
            return rows, False
        size = os.path.getsize(path)
        with open(path, "rb") as handle:
            if size > MAX_READ:
                handle.seek(size - MAX_READ)
                handle.readline()
                complete = False
            for raw in handle:
                try:
                    row = json.loads(raw.decode("utf-8"))
                except (ValueError, UnicodeError):
                    continue
                if isinstance(row, dict):
                    rows.append(row)
    except OSError:
        return [], True
    return rows, complete


def count(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value > 0 else 0


def build(plan, rows, complete):
    tasks = [task for task in plan["tasks"].values() if isinstance(task, dict) and isinstance(task.get("id"), str)]
    ids = {task["id"] for task in tasks}
    states = {}
    for task in tasks:
        state = clean(task.get("state"), 24) or "unknown"
        states[state] = states.get(state, 0) + 1

    cycles, runs = {}, set()
    for row in rows:
        if row.get("kind") in CYCLE_KINDS and row.get("task_id") in ids:
            cycles[row["task_id"]] = cycles.get(row["task_id"], 0) + 1
            if isinstance(row.get("run_id"), str):
                runs.add(row["run_id"])

    retried = []
    for task in tasks:
        history = task.get("history") if isinstance(task.get("history"), list) else []
        amended = sum(1 for item in history if isinstance(item, dict) and item.get("kind") == "amended")
        reopened = sum(1 for item in history if isinstance(item, dict)
                       and item.get("kind") == "completion-reactivated")
        repairs, spent = count(task.get("repair_count")), cycles.get(task["id"], 0)
        if repairs or amended or reopened or spent > 1:
            retried.append({"id": task["id"], "title": clean(task.get("title"), 80), "cycles": spent,
                            "repairs": repairs, "amended": amended, "reopened": reopened})
    retried.sort(key=lambda item: (-(item["cycles"] + item["repairs"] + item["amended"] + item["reopened"]),
                                   item["id"]))

    def held(state):
        return [{"id": task["id"], "title": clean(task.get("title"), 80), "reason": clean(task.get("reason"))}
                for task in tasks if task.get("state") == state]

    plan_id = plan.get("plan_id") if isinstance(plan.get("plan_id"), str) else None
    acceptance = {"pass": 0, "fail": 0, "other": 0, "reasons": {}}
    for row in rows:
        if row.get("kind") != "acceptance":
            continue
        # A run of the drive on this plan, or an acceptance with no run at all, belongs here;
        # a row from a run this plan's cycles never named belongs to an earlier plan.
        if isinstance(row.get("run_id"), str) and row["run_id"] not in runs:
            continue
        status = row.get("status")
        acceptance["pass" if status == "pass" else "fail" if status == "fail" else "other"] += 1
        reason = clean(row.get("reason"), 80)
        if reason and status != "pass":
            acceptance["reasons"][reason] = acceptance["reasons"].get(reason, 0) + 1

    parks = {}
    finished = 0
    for row in rows:
        if row.get("kind") != "terminal" or row.get("run_id") not in runs:
            continue
        if row.get("status") == "park":
            reason = clean(row.get("reason"), 80) or "unstated"
            parks[reason] = parks.get(reason, 0) + 1
        elif row.get("status") == "done":
            finished += 1

    blocked, cancelled = held("blocked"), held("cancelled")
    return {
        "schema": 1, "kind": "oms-plan-retro",
        "goal": clean(plan.get("goal"), 200), "plan_id": plan_id,
        "tasks": {"total": len(tasks), "states": dict(sorted(states.items()))},
        "retried": retried[:MAX_ROWS], "retried_omitted": max(0, len(retried) - MAX_ROWS),
        "blocked": blocked[:MAX_ROWS], "blocked_omitted": max(0, len(blocked) - MAX_ROWS),
        "cancelled": cancelled[:MAX_ROWS], "cancelled_omitted": max(0, len(cancelled) - MAX_ROWS),
        "drive": {"runs": len(runs), "cycles": sum(cycles.values()), "finished": finished,
                  "parks": dict(sorted(parks.items(), key=lambda item: (-item[1], item[0])))},
        "acceptance": {"pass": acceptance["pass"], "fail": acceptance["fail"], "other": acceptance["other"],
                       "reasons": dict(sorted(acceptance["reasons"].items(),
                                              key=lambda item: (-item[1], item[0]))[:MAX_ROWS])},
        "ledger_complete": complete,
        "not_counted": ["review finding verdicts", "criteria whose evidence went stale"],
    }


def render(report):
    lines = ["plan retro: %s" % (report["goal"] or "(no goal recorded)")]
    states = report["tasks"]["states"]
    lines.append("tasks: %d (%s)" % (report["tasks"]["total"],
                                     ", ".join("%s=%d" % item for item in states.items()) or "none"))
    drive = report["drive"]
    lines.append("drive: %d run(s), %d cycle(s), %d finished" % (drive["runs"], drive["cycles"], drive["finished"]))
    for reason, times in drive["parks"].items():
        lines.append("  parked %dx: %s" % (times, reason))
    accept = report["acceptance"]
    lines.append("acceptance: %d pass, %d fail%s" % (
        accept["pass"], accept["fail"], ", %d other" % accept["other"] if accept["other"] else ""))
    for reason, times in accept["reasons"].items():
        lines.append("  not passing %dx: %s" % (times, reason))
    lines.append("took more than one try: %s" % ("none recorded" if not report["retried"] else ""))
    for item in report["retried"]:
        parts = [name + "=" + str(item[name]) for name in ("cycles", "repairs", "amended", "reopened") if item[name]]
        lines.append("  %s  %s  %s" % (item["id"], ", ".join(parts), item["title"]))
    if report["retried_omitted"]:
        lines.append("  +%d more" % report["retried_omitted"])
    for label in ("blocked", "cancelled"):
        if report[label]:
            lines.append("%s:" % label)
            for item in report[label]:
                lines.append("  %s  %s  [%s]" % (item["id"], item["title"], item["reason"] or "no reason recorded"))
            if report[label + "_omitted"]:
                lines.append("  +%d more" % report[label + "_omitted"])
    if not report["ledger_complete"]:
        lines.append("incomplete: the progress ledger is longer than the read cap; only its newest part was counted")
    lines.append("not counted: " + "; ".join(report["not_counted"]))
    lines.append("counts of recorded rows only: not approval or verification evidence, and nothing was written; "
                 "record what should outlive this plan yourself (PROJECT.md Decisions, oms agent-memory)")
    return "\n".join(lines)


def main(argv):
    as_json = "--json" in argv[1:]
    paths = [arg for arg in argv[1:] if arg != "--json"]
    if len(paths) != 1:
        print("usage: plan_retro.py PLAN_FILE [--json]", file=sys.stderr)
        return 2
    plan = read_plan(paths[0])
    if plan is None:
        print("error: no readable plan at .oms/plan/tasks.json; a retired plan has no retro "
              "(run it before agent-plan retire)", file=sys.stderr)
        return 2
    rows, complete = read_progress(os.path.join(os.path.dirname(paths[0]), "progress.jsonl"))
    report = build(plan, rows, complete)
    print(json.dumps(report, ensure_ascii=False, sort_keys=True) if as_json else render(report))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
