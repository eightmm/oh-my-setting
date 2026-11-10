"""What a finished Slurm job consumed, from sacct's own allocation records.

Only arithmetic over reported fields: GPU-hours are allocated GPUs times elapsed
time, queue wait is start minus submit, time efficiency is elapsed over the
requested limit and CPU efficiency is TotalCPU over allocated CPUs times
elapsed. sacct does not report GPU utilisation, and peak memory is a step
field the allocation query does not read, so both stay unknown rather than
estimated. A field sacct did not give leaves its figure out.
"""

import argparse
import datetime
import json
import re
import sys

UNKNOWN = ("", "Unknown", "None", "N/A", "UNLIMITED", "Partition_Limit", "INVALID")


def seconds(text):
    """Slurm duration ([D-]HH:MM:SS[.fff] or MM:SS[.fff]) as whole seconds, None when unreadable."""
    text = (text or "").strip()
    if text in UNKNOWN:
        return None
    match = re.fullmatch(r"(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+)(?:\.\d+)?", text)
    if not match:
        return None
    days, hours, minutes, secs = (int(part or 0) for part in match.groups())
    return ((days * 24 + hours) * 60 + minutes) * 60 + secs


def stamp(text):
    """sacct timestamp (YYYY-MM-DDTHH:MM:SS, cluster local time) as naive seconds, None when unreadable."""
    text = (text or "").strip()
    if text in UNKNOWN:
        return None
    try:
        moment = datetime.datetime.strptime(text[:19], "%Y-%m-%dT%H:%M:%S")
    except ValueError:
        return None
    return int((moment - datetime.datetime(1970, 1, 1)).total_seconds())


def tres_count(alloc_tres, key):
    """Count for one AllocTRES key; None when the allocation was not reported, 0 when it lists no such resource."""
    text = (alloc_tres or "").strip()
    if not text:
        return None
    exact, typed = None, 0
    for part in text.split(","):
        name, _, value = part.partition("=")
        name = name.strip()
        try:
            number = int(value.strip())
        except ValueError:
            continue
        if name == key:
            exact = number
        elif name.startswith(key + ":"):
            typed += number
    return exact if exact is not None else typed


def consumed(records):
    """Usage of one job from all of its allocation records (every array element and every requeue attempt).

    `records` are dicts with elapsed, submit, start, alloc_tres and optionally timelimit, alloc_cpus, total_cpu.
    Returns {} when no figure could be computed.
    """
    out = {}
    elapsed = [seconds(row.get("elapsed")) for row in records]
    known = [value for value in elapsed if value is not None]
    if known and len(known) == len(records):
        out["elapsed_seconds"] = sum(known)
    waits = []
    for row in records:
        begin, submit = stamp(row.get("start")), stamp(row.get("submit"))
        if begin is not None and submit is not None and begin >= submit:
            waits.append(begin - submit)
    if waits:
        out["queue_wait_seconds"] = sum(waits)
        if len(waits) != len(records):
            out["queue_wait_records"] = len(waits)
    gpu_seconds, complete = 0, bool(records)
    for row, spent in zip(records, elapsed):
        gpus = tres_count(row.get("alloc_tres"), "gres/gpu")
        if gpus is None or spent is None:
            complete = False
            break
        gpu_seconds += gpus * spent
    if complete:
        out["gpu_hours"] = round(gpu_seconds / 3600.0, 4)
    limit_total, spent_total, cpu_used, cpu_given = 0, 0, 0, 0
    limits_ok = cpus_ok = bool(records)
    for row, spent in zip(records, elapsed):
        limit = seconds(row.get("timelimit"))
        if limit is None or spent is None or limit <= 0:
            limits_ok = False
        else:
            limit_total, spent_total = limit_total + limit, spent_total + spent
        used = seconds(row.get("total_cpu"))
        try:
            cpus = int(row.get("alloc_cpus") or 0)
        except ValueError:
            cpus = 0
        if used is None or spent is None or cpus <= 0 or spent <= 0:
            cpus_ok = False
        else:
            cpu_used, cpu_given = cpu_used + used, cpu_given + cpus * spent
    if limits_ok and limit_total:
        out["time_efficiency"] = round(spent_total / float(limit_total), 4)
    if cpus_ok and cpu_given:
        out["cpu_efficiency"] = round(cpu_used / float(cpu_given), 4)
    return out


def clock(total):
    hours, rest = divmod(int(total), 3600)
    return "%02d:%02d:%02d" % (hours, rest // 60, rest % 60)


def experiment_jobs(ledger, experiment):
    """Job ids of ledger rows whose note carries `experiment=ID` (how contract runs are ledgered)."""
    wanted = re.compile(r"(^|\s)experiment=%s(\s|$)" % re.escape(experiment))
    found = []
    try:
        handle = open(ledger, encoding="utf-8", errors="replace")
    except OSError:
        return found
    with handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            job = str(row.get("slurm_job_id") or "").strip() if isinstance(row, dict) else ""
            if job and job not in found and wanted.search(str(row.get("note") or "")):
                found.append(job)
    return found


def summary(reconcile, jobs=None):
    """Totals over the reconcile rows, one row per (job id, submit time)."""
    rows, seen = [], set()
    try:
        handle = open(reconcile, encoding="utf-8", errors="replace")
    except OSError:
        handle = None
    if handle is not None:
        with handle:
            for line in handle:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(row, dict) or not row.get("job_id"):
                    continue
                key = (str(row["job_id"]), str(row.get("submit") or ""))
                if key in seen or (jobs is not None and key[0] not in jobs):
                    continue
                seen.add(key)
                rows.append(row)
    usage = [row.get("usage") if isinstance(row.get("usage"), dict) else {} for row in rows]

    def total(key):
        values = [item[key] for item in usage if isinstance(item.get(key), (int, float))]
        return (round(sum(values), 4), len(values)) if values else (None, 0)

    def mean(key):
        values = [item[key] for item in usage if isinstance(item.get(key), (int, float))]
        return (round(sum(values) / len(values), 4), len(values)) if values else (None, 0)

    gpu, gpu_n = total("gpu_hours")
    wait, wait_n = total("queue_wait_seconds")
    spent, spent_n = total("elapsed_seconds")
    time_eff, time_n = mean("time_efficiency")
    cpu_eff, cpu_n = mean("cpu_efficiency")
    states = {}
    for row in rows:
        states[str(row.get("state"))] = states.get(str(row.get("state")), 0) + 1
    return {"schema": 1, "kind": "oms-run-usage", "jobs": len(rows), "states": states,
            "gpu_hours": gpu, "gpu_hours_jobs": gpu_n,
            "queue_wait_seconds": wait, "queue_wait_jobs": wait_n,
            "elapsed_seconds": spent, "elapsed_jobs": spent_n,
            "time_efficiency_mean": time_eff, "time_efficiency_jobs": time_n,
            "cpu_efficiency_mean": cpu_eff, "cpu_efficiency_jobs": cpu_n,
            "gpu_utilization": "unknown", "memory_efficiency": "unknown"}


def render(report, scope):
    jobs = report["jobs"]
    lines = ["run usage%s: %d recorded job(s)%s" % (
        scope, jobs,
        " (%s)" % ", ".join("%s=%s" % item for item in sorted(report["states"].items())) if jobs else "")]

    def missing(count):
        return "; %d job(s) unknown" % (jobs - count) if jobs - count else ""

    if report["gpu_hours"] is None:
        lines.append("gpu-hours: unknown (no recorded job carries its allocation and elapsed time)")
    else:
        lines.append("gpu-hours: %s allocated over %d job(s)%s" % (
            report["gpu_hours"], report["gpu_hours_jobs"], missing(report["gpu_hours_jobs"])))
    if report["queue_wait_seconds"] is None:
        lines.append("queue wait: unknown")
    else:
        lines.append("queue wait: %s total over %d job(s)%s" % (
            clock(report["queue_wait_seconds"]), report["queue_wait_jobs"], missing(report["queue_wait_jobs"])))
    if report["elapsed_seconds"] is not None:
        lines.append("elapsed: %s total over %d job(s)%s" % (
            clock(report["elapsed_seconds"]), report["elapsed_jobs"], missing(report["elapsed_jobs"])))
    for label, key in (("time efficiency (elapsed / requested limit)", "time_efficiency"),
                       ("cpu efficiency (TotalCPU / allocated cpu time)", "cpu_efficiency")):
        value, count = report[key + "_mean"], report[key + "_jobs"]
        lines.append("%s: %s" % (label, "unknown" if value is None else "%.0f%% mean over %d job(s)%s" % (
            value * 100, count, missing(count))))
    lines.append("gpu utilisation: unknown (sacct reports the allocation, not how busy the GPU was)")
    lines.append("memory efficiency: unknown (peak memory is a step field this query does not read)")
    lines.append("allocated figures from recorded accounting rows only; nothing was queried or re-run")
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(prog="run-reconcile.sh usage")
    parser.add_argument("--reconcile", required=True)
    parser.add_argument("--ledger", required=True)
    parser.add_argument("--experiment", default="")
    parser.add_argument("--job", default="")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    jobs, scope = None, ""
    if args.experiment:
        jobs, scope = set(experiment_jobs(args.ledger, args.experiment)), " for experiment %s" % args.experiment
    if args.job:
        jobs = {args.job} if jobs is None else jobs & {args.job}
        scope += " for job %s" % args.job
    report = summary(args.reconcile, jobs)
    if args.experiment:
        report["experiment"] = args.experiment
    print(json.dumps(report, ensure_ascii=False, sort_keys=True) if args.json else render(report, scope))
    return 0


if __name__ == "__main__":
    sys.exit(main())
