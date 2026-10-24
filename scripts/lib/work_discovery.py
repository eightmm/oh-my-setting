"""Collect a bounded, untrusted evidence packet for work planning."""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import panel_results
SECTION_BUDGETS = {"failures": 4096, "metrics": 4096, "reviews": 65536,
                   "ci": 4096, "tasks": 4096, "rejected": 32768}
TOTAL_BUDGET = 131072
SECTIONS = tuple(SECTION_BUDGETS)
_TIMED_OUT = object()


class _RunFailure:
    def __init__(self, status, stderr):
        self.status = status
        self.stderr = stderr


class DiscoveryError(Exception):
    """Base class for packet collection errors."""


class BudgetError(DiscoveryError):
    def __init__(self, section, size, budget):
        self.section, self.size, self.budget = section, size, budget
        super().__init__("%s is %d bytes, over the %d-byte budget" %
                         (section, size, budget))


class CorruptEvidenceError(DiscoveryError):
    def __init__(self, section):
        self.section = section
        super().__init__(section)


def _clean(value):
    if isinstance(value, str):
        value = value.replace("\r", "")
        if panel_results._contains_outbound_machine_path(value):
            return None
        return value
    if isinstance(value, list):
        return [cleaned for item in value if (cleaned := _clean(item)) is not None]
    if isinstance(value, dict):
        return {key: cleaned for key, item in value.items()
                if (cleaned := _clean(item)) is not None}
    return value


def _run(repo, name, *args, optional=False):
    path = Path(os.environ.get("OMS_WORK_DISCOVERY_SCRIPTS_DIR", HERE.parent)) / name
    if not path.is_file():
        if optional:
            return None
        raise CorruptEvidenceError(name)
    try:
        command = ["bash", str(path)]
        if name != "ci-status.sh":
            command += ["--repo", str(repo)]
        command += list(args)
        result = subprocess.run(command, cwd=str(repo),
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               universal_newlines=True, timeout=30, check=False)
    except subprocess.TimeoutExpired:
        if optional:
            return _TIMED_OUT
        raise CorruptEvidenceError(name)
    except (OSError, UnicodeError, subprocess.SubprocessError):
        if optional:
            return None
        raise CorruptEvidenceError(name)
    if result.returncode:
        if optional:
            first_line = result.stderr.replace("\r", "").splitlines()
            return _RunFailure(result.returncode, first_line[0] if first_line else "")
        raise CorruptEvidenceError(name)
    return result.stdout.replace("\r", "")


def _section(items, coverage=None, reason=None):
    return {"status": "available" if reason is None else "unavailable",
            "reason": reason, "coverage": coverage or {}, "items": items}


def _fingerprint(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _collect_reviews(repo):
    try:
        attempts, rows = panel_results._records(repo, full=True)
    except subprocess.TimeoutExpired:
        return _section([], {"complete": False}, "timeout")
    except (OSError, ValueError, subprocess.SubprocessError):
        return _section([], {"complete": False}, "review evidence unavailable")
    items = []
    artifact_coverage = True
    unreadable_results = 0
    attempts_by_id = {row.get("attempt_id"): row for row in attempts if isinstance(row, dict)}
    review_roles = {"reviewer", "verifier", "critic", "security-auditor"}
    review_rows = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        attempt = attempts_by_id.get(row.get("attempt_id"), {})
        role = attempt.get("refs", {}).get("panel_role") if isinstance(attempt.get("refs"), dict) else None
        if role in review_roles:
            review_rows.append(row)
    task_ids = {row.get("task_id") for row in review_rows if isinstance(row.get("task_id"), str)}
    try:
        decisions = panel_results.outcomes(repo, task_ids)
    except subprocess.TimeoutExpired:
        return _section([], {"complete": False, "considered": len(review_rows), "selected": 0},
                        "timeout")
    except (OSError, ValueError, subprocess.SubprocessError):
        return _section([], {"complete": False, "considered": len(review_rows), "selected": 0},
                        "owner decisions unavailable")
    selected = [row for row in review_rows if row.get("task_id") not in decisions]
    for row in selected:
        artifact_id = _clean(row.get("artifact_id"))
        ref = artifact_id or _clean(row.get("attempt_id"))
        if ref:
            match = re.fullmatch(r"sha256:([0-9a-f]{64})", str(ref))
            item = {"ref": match.group(1)[:16] if match else str(ref),
                    "artifact_id": str(ref), "kind": row.get("kind"),
                    "attempt_id": row.get("attempt_id"), "exit": row.get("exit"),
                    "outcome": row.get("outcome"), "task_id": row.get("task_id")}
            if isinstance(row.get("artifact"), str):
                try:
                    with contextlib.redirect_stderr(io.StringIO()):
                        content = panel_results._indexed(repo, row).decode("utf-8")
                    output, _ = panel_results.artifact_sections_stream(
                        io.BytesIO(content.encode("utf-8")))
                    _, sections = panel_results.debate_sections(output)
                    item["findings"] = next((body.strip() for heading, body in sections
                                             if heading == "Findings"), "")
                except (OSError, ValueError, UnicodeError):
                    artifact_coverage = False
                    unreadable_results += 1
            items.append(item)
    return _section(items, {"complete": artifact_coverage, "considered": len(review_rows),
                            "selected": len(items), "unreadable_results": unreadable_results})


def _collect_metrics(repo):
    entry = Path(os.environ.get("OMS_WORK_DISCOVERY_OMS", ROOT / "scripts/oms"))
    try:
        proc = subprocess.run(["bash", str(entry), "panel", "--metrics", "--json"], cwd=str(repo),
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         universal_newlines=True, timeout=30, check=False)
    except subprocess.TimeoutExpired:
        return _section([], {}, "timeout")
    if proc.returncode:
        return _section([], {}, "metrics source unavailable")
    try:
        report = json.loads(proc.stdout.replace("\r", ""))
    except ValueError:
        return _section([], {}, "metrics source returned invalid JSON")
    if not isinstance(report, dict):
        return _section([], {}, "metrics source returned invalid JSON")
    writes = report.get("write_attempts") if isinstance(report.get("write_attempts"), dict) else {}
    failure_data = writes.get("failed_and_undecided") if isinstance(writes.get("failed_and_undecided"), dict) else {}
    interventions = report.get("interventions") if isinstance(report.get("interventions"), dict) else {}
    raw_coverage = report.get("coverage") if isinstance(report.get("coverage"), dict) else {}
    projected = {
        "window": report.get("window", {"since": report.get("since"), "until": report.get("until")}),
        "coverage": {key: raw_coverage[key] for key in
                     ("incomplete", "attempts_observed", "artifact_events_observed",
                      "attempt_limit", "artifact_limit") if key in raw_coverage},
        "incomplete": bool(report.get("incomplete")),
        "write_state_reconciliation": {"total": writes.get("total"), "states": writes.get("states"),
                                       "reconciles": writes.get("reconciles")},
        "failure_reason_counts": failure_data.get("reason_code", {}),
        "failure_stage_buckets": failure_data.get("first_failing_stage", {}),
        "cancel_totals": {"cancels": interventions.get("cancels"),
                          "total": interventions.get("total")},
    }
    return _section([_clean(projected)], {"complete": not bool(report.get("incomplete"))})


def collect(repo):
    repo = Path(repo).resolve()
    sections = {}

    raw = _run(repo, "fail-ledger.sh", "list", "--unresolved", "--json", optional=True)
    if raw is _TIMED_OUT:
        sections["failures"] = _section([], {}, "timeout")
    elif isinstance(raw, _RunFailure):
        detail = _clean(raw.stderr)
        reason = "fail-ledger exited %d" % raw.status
        if detail:
            reason += ": " + detail
        sections["failures"] = _section([], {}, reason)
    elif raw is None:
        sections["failures"] = _section([], {}, "fail-ledger unavailable")
    else:
        try:
            doc = json.loads(raw)
            rows = (doc.get("items", doc.get("failures", []))
                    if isinstance(doc, dict) else doc)
            if not isinstance(rows, list):
                raise ValueError()
            items = []
            for row in rows:
                if not isinstance(row, dict):
                    continue
                cleaned = _clean({key: row.get(key) for key in
                                  ("fingerprint", "count", "last_exit", "summary")})
                if cleaned is None:
                    continue
                ref = cleaned.get("fingerprint") or _fingerprint(json.dumps(cleaned, sort_keys=True))
                cleaned["ref"] = str(ref)
                items.append(cleaned)
            sections["failures"] = _section(items, {"count": len(rows), "complete": True})
        except (ValueError, TypeError):
            sections["failures"] = _section([], {}, "fail-ledger returned invalid JSON")

    sections["metrics"] = _collect_metrics(repo)
    sections["reviews"] = _collect_reviews(repo)

    raw = _run(repo, "ci-status.sh", "history", optional=True)
    if raw is _TIMED_OUT:
        sections["ci"] = _section([], {}, "timeout")
    elif raw is None:
        sections["ci"] = _section([], {"stage_timing": "unavailable"}, "CI history unavailable")
    elif isinstance(raw, _RunFailure):
        detail = _clean(raw.stderr)
        reason = "CI history exited %d" % raw.status
        if detail:
            reason += ": " + detail
        sections["ci"] = _section([], {"stage_timing": "unavailable"}, reason)
    else:
        items = []
        for line in raw.splitlines():
            if not line.strip() or line.startswith(("Failing jobs", "Failing tests", "  ")):
                continue
            safe_line = _clean(line)
            if not safe_line:
                continue
            sha = safe_line.split()[0]
            items.append({"ref": sha, "summary": safe_line})
        sections["ci"] = _section(items, {"stage_timing": "unavailable", "complete": True})

    try:
        raw = _run(repo, "agent-plan.sh", "list", "--json")
    except CorruptEvidenceError as error:
        raise CorruptEvidenceError("tasks") from error
    try:
        tasks = json.loads(raw)
        if isinstance(tasks, dict):
            tasks = tasks.get("tasks", [])
        if isinstance(tasks, dict):
            tasks = list(tasks.values())
        if not isinstance(tasks, list) or any(not isinstance(row, dict) for row in tasks):
            raise ValueError()
    except (ValueError, TypeError):
        raise CorruptEvidenceError("tasks")
    task_items = []
    for row in tasks:
        cleaned = _clean({key: row.get(key) for key in ("id", "state", "title", "depends")})
        if cleaned and cleaned.get("id"):
            task_items.append(cleaned)
    sections["tasks"] = _section(task_items,
        {"count": len(tasks), "open": sum(row.get("state") not in {"done", "cancelled"} for row in tasks),
         "terminal": sum(row.get("state") in {"done", "cancelled"} for row in tasks)})

    try:
        raw = _run(repo, "agent-memory.sh", "--full", "show")
    except CorruptEvidenceError as error:
        raise CorruptEvidenceError("rejected") from error
    try:
        matches = []
        for number, line in enumerate(raw.splitlines(), 1):
            if line.startswith("hypothesis:"):
                anchor = "line-%d-%s" % (number, _fingerprint(line)[:16])
                matches.append({"ref": anchor, "line": number, "text": line})
        sections["rejected"] = _section(matches, {"complete": True, "hypotheses": len(matches)})
    except (UnicodeError, ValueError):
        raise CorruptEvidenceError("rejected")

    packet = {"schema": 1, "sections": {name: _clean(sections[name]) for name in SECTIONS}}
    for name in SECTIONS:
        size = len(json.dumps(packet["sections"][name], ensure_ascii=False,
                              separators=(",", ":")).encode("utf-8"))
        if size > SECTION_BUDGETS[name]:
            raise BudgetError(name, size, SECTION_BUDGETS[name])
    packet["bytes"] = 0
    while True:
        size = len(json.dumps(packet, ensure_ascii=False,
                              separators=(",", ":")).encode("utf-8"))
        if packet["bytes"] == size:
            break
        packet["bytes"] = size
    if packet["bytes"] > TOTAL_BUDGET:
        raise BudgetError("total", packet["bytes"], TOTAL_BUDGET)
    return packet


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if len(argv) != 3 or argv[0] != "collect" or argv[1] != "--repo":
        print("usage: work_discovery.py collect --repo PATH", file=sys.stderr)
        return 2
    try:
        print(json.dumps(collect(argv[2]), ensure_ascii=False, separators=(",", ":")))
        return 0
    except BudgetError as error:
        print("work-discovery: " + str(error), file=sys.stderr)
        return 2
    except CorruptEvidenceError as error:
        print("work-discovery: corrupt " + error.section + " evidence", file=sys.stderr)
        return 3


if __name__ == "__main__":
    raise SystemExit(main())
