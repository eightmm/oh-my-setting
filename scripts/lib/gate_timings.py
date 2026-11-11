#!/usr/bin/env python3
"""Where the verification time goes: local gate stages and CI jobs, from logs.

Reads what the gate and CI already print and writes nothing back. Inputs:

  --timings FILE    a check runtime `timings` file (TSV: seconds, kind, name)
  --gate-log FILE   a gate log (`ok: NAME (Ns)`, `check: NAME FAILED (Ns)`,
                    `smoke-timing:`, `operator-section:`, `operator-shard:`,
                    and an optional trailing `exit=N wall=S` line)
  --ci-jobs FILE    `gh api repos/OWNER/REPO/actions/runs/ID/jobs?per_page=100`
  --ci-run FILE     `gh api repos/OWNER/REPO/actions/runs/ID` (wall clock)
  --ci-logs DIR     job logs saved as `job-<job id>.log` (`gh api .../jobs/ID/logs`)

Output is text, or one JSON document with --json. Per-test seconds exist only
where a runner printed them: CI keeps the slowest 25 per scripts-smoke shard and
a local gate prints none unless OMS_SMOKE_TIMINGS=1 reaches the runner.
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime

STAMP = re.compile(r"^﻿?\d{4}-\d\d-\d\dT[0-9:.]+Z ")
STAGE_OK = re.compile(r"^ok: (\S.*?) \((\d+)s\)$")
STAGE_FAIL = re.compile(r"^check: (\S.*?) FAILED \((\d+)s\)$")
TEST = re.compile(r"^smoke-timing: (\d+)s ([A-Za-z0-9_]+)$")
SLOW_TEST = re.compile(r"^slow-test: (\d+)s ([A-Za-z0-9_]+)$")
SLOW_STAGE = re.compile(r"^slow-stage: (\d+)s (\S.*)$")
SECTION = re.compile(r"^operator-section: (\d+)s ([A-Za-z0-9_./-]+)$")
SHARD = re.compile(r"^operator-shard: (\d+/\d+) (\d+)s$")
WALL = re.compile(r"^exit=(\d+) wall=(\d+)$")


def parse_lines(lines):
    """Stages, tests, operator sections and shards found in one log."""
    out = {"stages": [], "tests": [], "sections": [], "shards": [], "exit": None, "wall": None}
    slow = []
    slow_stages = []
    for raw in lines:
        line = STAMP.sub("", raw.rstrip("\n").lstrip("﻿"))
        m = STAGE_OK.match(line)
        if m:
            out["stages"].append({"name": m.group(1), "seconds": int(m.group(2)), "status": "ok"})
            continue
        m = STAGE_FAIL.match(line)
        if m:
            out["stages"].append({"name": m.group(1), "seconds": int(m.group(2)), "status": "failed"})
            continue
        m = TEST.match(line)
        if m:
            out["tests"].append({"name": m.group(2), "seconds": int(m.group(1))})
            continue
        m = SLOW_TEST.match(line)
        if m:
            slow.append({"name": m.group(2), "seconds": int(m.group(1))})
            continue
        m = SLOW_STAGE.match(line)
        if m:
            slow_stages.append({"name": m.group(2), "seconds": int(m.group(1)), "status": "ok"})
            continue
        m = SECTION.match(line)
        if m:
            out["sections"].append({"name": m.group(2), "seconds": int(m.group(1))})
            continue
        m = SHARD.match(line)
        if m:
            out["shards"].append({"name": m.group(1), "seconds": int(m.group(2))})
            continue
        m = WALL.match(line)
        if m:
            out["exit"], out["wall"] = int(m.group(1)), int(m.group(2))
    # Parallel lanes print no `ok:` line; the summary's top-5 list is all they leave.
    seen = {row["name"] for row in out["stages"]}
    out["stages"] += [row for row in slow_stages if row["name"] not in seen]
    if not out["tests"]:
        out["tests"] = slow  # check-summary repeats the runner's list
    return out


def parse_timings(path):
    out = {"stages": [], "tests": [], "sections": [], "shards": [], "exit": None, "wall": None}
    keys = {"stage": "stages", "test": "tests", "operator-section": "sections", "operator-shard": "shards"}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t", 2)
            if len(parts) != 3 or parts[1] not in keys or not parts[0].isdigit():
                continue
            row = {"name": parts[2], "seconds": int(parts[0])}
            if parts[1] == "stage":
                row["status"] = "ok"
            out[keys[parts[1]]].append(row)
    return out


def ranked(rows, top):
    return sorted(rows, key=lambda r: (-r["seconds"], r["name"]))[:top]


def when(text):
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def ci_report(jobs_path, run_path, logs_dir, top):
    with open(jobs_path, encoding="utf-8") as handle:
        jobs = json.load(handle).get("jobs", [])
    report = {"jobs": [], "stages": [], "tests": [], "suites": [], "job_seconds_total": 0, "wall_seconds": None}
    suites = {}
    for job in jobs:
        if not job.get("started_at") or not job.get("completed_at") or job.get("conclusion") == "skipped":
            continue
        seconds = int((when(job["completed_at"]) - when(job["started_at"])).total_seconds())
        steps = []
        for step in job.get("steps") or []:
            if step.get("started_at") and step.get("completed_at"):
                steps.append({"name": step["name"],
                              "seconds": int((when(step["completed_at"]) - when(step["started_at"])).total_seconds())})
        row = {"id": job["id"], "name": job["name"], "conclusion": job.get("conclusion"),
               "seconds": seconds, "slowest_step": ranked(steps, 1)[0] if steps else None}
        report["jobs"].append(row)
        report["job_seconds_total"] += seconds
        log = os.path.join(logs_dir or "", "job-%s.log" % job["id"])
        if not logs_dir or not os.path.isfile(log):
            continue
        with open(log, encoding="utf-8", errors="replace") as handle:
            parsed = parse_lines(handle)
        stage_sum = 0
        for stage in parsed["stages"]:
            report["stages"].append(dict(stage, job=job["name"]))
            stage_sum += stage["seconds"]
            suites.setdefault(stage["name"], {"name": stage["name"], "seconds": 0, "jobs": 0})
            suites[stage["name"]]["seconds"] += stage["seconds"]
            suites[stage["name"]]["jobs"] += 1
        row["stage_seconds"] = stage_sum
        for test in parsed["tests"]:
            report["tests"].append(dict(test, job=job["name"]))
    report["jobs"].sort(key=lambda r: -r["seconds"])
    report["tests"] = ranked(report["tests"], top)
    report["suites"] = ranked(list(suites.values()), len(suites))
    if run_path:
        with open(run_path, encoding="utf-8") as handle:
            run = json.load(handle)
        report["wall_seconds"] = int((when(run["updated_at"]) - when(run.get("run_started_at") or run["created_at"])).total_seconds())
    return report


def local_report(args):
    parsed = None
    if args.timings:
        parsed = parse_timings(args.timings)
    if args.gate_log:
        with open(args.gate_log, encoding="utf-8", errors="replace") as handle:
            log = parse_lines(handle)
        if parsed is None:
            parsed = log
        else:
            parsed["exit"], parsed["wall"] = log["exit"], log["wall"]
    if parsed is None:
        return None
    parsed["stages"] = ranked(parsed["stages"], len(parsed["stages"]))
    parsed["stage_seconds_total"] = sum(r["seconds"] for r in parsed["stages"])
    parsed["tests"] = ranked(parsed["tests"], args.top)
    parsed["sections"] = ranked(parsed["sections"], args.top)
    return parsed


def fmt(rows, columns):
    lines = []
    for row in rows:
        lines.append("  " + "  ".join(str(row.get(c, "")).ljust(w) if i < len(columns) - 1 else str(row.get(c, ""))
                                      for i, (c, w) in enumerate(columns)))
    return lines


def text(report):
    lines = []
    local = report.get("local")
    if local:
        lines.append("local gate: stages by seconds (sum %ss%s)" % (
            local["stage_seconds_total"], "; wall %ss, exit %s" % (local["wall"], local["exit"]) if local["wall"] is not None else ""))
        lines += fmt(local["stages"], [("seconds", 6), ("status", 7), ("name", 0)])
        if local["sections"]:
            lines.append("local gate: operator sections")
            lines += fmt(local["sections"], [("seconds", 6), ("name", 0)])
        lines.append("local gate: slowest tests" + ("" if local["tests"] else " (none printed; set OMS_SMOKE_TIMINGS=1)"))
        lines += fmt(local["tests"], [("seconds", 6), ("name", 0)])
    ci = report.get("ci")
    if ci:
        lines.append("ci: jobs (sum of job time %ss%s)" % (
            ci["job_seconds_total"], "; wall clock %ss" % ci["wall_seconds"] if ci["wall_seconds"] is not None else ""))
        lines += fmt(ci["jobs"], [("seconds", 6), ("conclusion", 8), ("name", 0)])
        lines.append("ci: suite totals by stage name (seconds summed over jobs)")
        lines += fmt(ci["suites"], [("seconds", 6), ("jobs", 5), ("name", 0)])
        lines.append("ci: slowest tests the logs printed")
        lines += fmt(ci["tests"], [("seconds", 6), ("job", 22), ("name", 0)])
    return "\n".join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--timings")
    ap.add_argument("--gate-log")
    ap.add_argument("--ci-jobs")
    ap.add_argument("--ci-run")
    ap.add_argument("--ci-logs")
    ap.add_argument("--top", type=int, default=30)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    report = {}
    try:
        local = local_report(args)
        if local:
            report["local"] = local
        if args.ci_jobs:
            report["ci"] = ci_report(args.ci_jobs, args.ci_run, args.ci_logs, args.top)
    except (OSError, ValueError, KeyError) as exc:
        print("gate_timings: %s" % exc, file=sys.stderr)
        return 2
    if not report:
        print("gate_timings: give --timings, --gate-log or --ci-jobs", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2, sort_keys=True) if args.json else text(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
