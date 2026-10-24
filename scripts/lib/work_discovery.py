"""Collect a bounded, untrusted evidence packet for work planning."""

import contextlib
import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import panel_results
import land_receipts
SECTION_BUDGETS = {"failures": 4096, "metrics": 4096, "reviews": 65536,
                   "ci": 4096, "tasks": 4096, "rejected": 32768}
TOTAL_BUDGET = 131072
SECTIONS = tuple(SECTION_BUDGETS)
_TIMED_OUT = object()
_LAND_DIR_LIMIT = 128
_LAND_PARENT_ENTRY_LIMIT = 2048
_LAND_ENTRY_LIMIT = 512
_LAND_RECEIPT_LIMIT = 256
_LAND_LOG_LIMIT = 1024 * 1024
_LAND_TOTAL_LOG_LIMIT = 8 * 1024 * 1024
_LAND_FAILURE_LIMIT = 16
_ATTEMPT_LINE = re.compile(
    rb"^land attempt: ([1-9][0-9]*) started_at=([0-9]{4}-[0-9]{2}-[0-9]{2}T"
    rb"[0-9]{2}:[0-9]{2}:[0-9]{2}Z) sha=([0-9a-f]{40}|[0-9a-f]{64})$")
_FAILED_CHECK = re.compile(
    rb"^check: ([A-Za-z0-9_.:-]{1,160}|affected:tests/[A-Za-z0-9_./-]{1,128}) FAILED(?:\s|$)")


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
    identity = json.dumps(projected, ensure_ascii=False, sort_keys=True,
                          separators=(",", ":"))
    projected["ref"] = "metrics-" + _fingerprint(identity)[:12]
    return _section([_clean(projected)], {"complete": not bool(report.get("incomplete"))})


def _land_state_directory(repo):
    helper = ROOT / "scripts/lib/land-receipts.sh"
    proc = subprocess.run(
        ["bash", "-c", '. "$1"; oms_land_state_dir "$2"', "oms-land-state",
         str(helper), str(repo)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        universal_newlines=True, timeout=10, check=False)
    if proc.returncode:
        raise OSError("land state directory unavailable")
    return Path(proc.stdout.replace("\r", "").strip())


def _read_land_file(directory, name, maximum, reader):
    if not name or name in {".", ".."} or "/" in name or "\\" in name:
        raise ValueError("invalid land filename")
    captured = io.StringIO()
    try:
        with contextlib.redirect_stderr(captured):
            return reader(str(directory), name, "land evidence", max_bytes=maximum)
    except SystemExit as error:
        raise ValueError(captured.getvalue() or str(error)) from None


def _land_attempt_failures(raw, expected_sha):
    attempts = []
    current = None
    incomplete = False
    next_attempt = 1
    for line in raw.splitlines():
        marker = _ATTEMPT_LINE.fullmatch(line)
        if marker:
            if current is not None:
                attempts.append(current)
            if int(marker.group(1)) != next_attempt:
                incomplete = True
            next_attempt = int(marker.group(1)) + 1
            stamp = marker.group(2).decode("ascii")
            try:
                date = datetime.datetime.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ").date().isoformat()
            except ValueError:
                current = None
                incomplete = True
                continue
            if marker.group(3).decode("ascii") != expected_sha:
                current = None
                incomplete = True
                continue
            current = {"number": int(marker.group(1)), "timestamp": stamp,
                       "date": date, "failures": set()}
            continue
        if line.startswith(b"land attempt:"):
            incomplete = True
        if current is None:
            continue
        match = _FAILED_CHECK.match(line)
        if match:
            identifier = match.group(1).decode("ascii")
            if identifier.startswith("affected:") and any(
                    part in {"", ".", ".."} for part in identifier[len("affected:"):].split("/")):
                incomplete = True
                continue
            kind = ("stage" if identifier.startswith("affected:") else
                    "test" if ".test_" in identifier or ".test-" in identifier else "stage")
            current["failures"].add((kind, identifier))
    if current is not None:
        attempts.append(current)
    if not attempts or (raw and not raw.endswith(b"\n")):
        incomplete = True
    return attempts, incomplete


def _collect_land_failures(repo):
    coverage = {"land_scan_complete": True, "land_receipts_considered": 0,
                "land_receipts_admitted": 0, "land_logs_missing": 0,
                "land_logs_unreadable": 0, "land_logs_oversized": 0,
                "land_logs_incomplete": 0,
                "association": "exact configured push URL, remote, target, and gate; renamed remotes or SSH/HTTPS aliases may be missed"}
    try:
        own = _land_state_directory(repo)
        if own.is_symlink() or (own.exists() and not own.is_dir()):
            raise OSError("land state directory is not a real directory")
        durable = runpy.run_path(str(HERE / "durable-jsonl.py"))["read_no_follow"]
        scan_dirs = [own] if own.exists() else []
        parent = own.parent
        seen_dirs = {os.path.abspath(own)}
        try:
            with os.scandir(parent) as entries:
                for index, entry in enumerate(entries):
                    if index >= _LAND_PARENT_ENTRY_LIMIT:
                        coverage["land_scan_complete"] = False
                        break
                    if len(scan_dirs) >= _LAND_DIR_LIMIT:
                        coverage["land_scan_complete"] = False
                        break
                    try:
                        if entry.is_dir(follow_symlinks=False) and os.path.abspath(entry.path) not in seen_dirs:
                            scan_dirs.append(Path(entry.path))
                            seen_dirs.add(os.path.abspath(entry.path))
                    except OSError:
                        coverage["land_scan_complete"] = False
        except OSError:
            coverage["land_scan_complete"] = False
        scan_dirs.sort(key=lambda directory: directory.name)
        aggregates = {}
        seen_receipts = set()
        requests_with_logs = set()
        seen_failure_attempts = set()
        missing_logs = set()
        total_log_bytes = 0
        receipt_count = 0
        for directory in scan_dirs:
            try:
                if directory.is_symlink() or not directory.is_dir():
                    coverage["land_scan_complete"] = False
                    continue
                with os.scandir(directory) as entries:
                    for index, entry in enumerate(entries):
                        if index >= _LAND_ENTRY_LIMIT:
                            coverage["land_scan_complete"] = False
                            break
                        if receipt_count >= _LAND_RECEIPT_LIMIT:
                            coverage["land_scan_complete"] = False
                            break
                        name = entry.name
                        if not name.endswith(".json") or not re.fullmatch(
                                r"[0-9a-f]{40,64}-[0-9a-f]{24}\.json", name):
                            continue
                        receipt_count += 1
                        coverage["land_receipts_considered"] += 1
                        try:
                            raw_receipt = _read_land_file(directory, name, 1024 * 1024, durable)
                            receipt = json.loads(raw_receipt)
                            if (not isinstance(receipt, dict) or type(receipt.get("schema")) is not int or
                                    receipt.get("schema") != 1 or
                                    not land_receipts.sibling_receipt_binds_repo(str(repo), receipt, name)):
                                continue
                            if name not in seen_receipts:
                                seen_receipts.add(name)
                                coverage["land_receipts_admitted"] += 1
                            sha = receipt.get("sha")
                            if total_log_bytes >= _LAND_TOTAL_LOG_LIMIT:
                                coverage["land_scan_complete"] = False
                                continue
                            log_name = name[:-5] + ".log"
                            try:
                                log = _read_land_file(directory, log_name,
                                                      min(_LAND_LOG_LIMIT,
                                                          _LAND_TOTAL_LOG_LIMIT - total_log_bytes), durable)
                            except ValueError as error:
                                message = str(error)
                                if "exceeds" in message or "size limit" in message:
                                    coverage["land_logs_oversized"] += 1
                                    coverage["land_scan_complete"] = False
                                elif "does not exist" in message or "No such file" in message:
                                    missing_logs.add(name)
                                else:
                                    coverage["land_logs_unreadable"] += 1
                                    coverage["land_scan_complete"] = False
                                continue
                            except (OSError, ValueError):
                                coverage["land_logs_unreadable"] += 1
                                coverage["land_scan_complete"] = False
                                continue
                            total_log_bytes += len(log)
                            requests_with_logs.add(name)
                            attempts, incomplete = _land_attempt_failures(log, sha)
                            if incomplete:
                                coverage["land_logs_incomplete"] += 1
                                coverage["land_scan_complete"] = False
                            for attempt in attempts:
                                for kind, identifier in attempt["failures"]:
                                    observation = (name, attempt["number"], attempt["timestamp"],
                                                   kind, identifier)
                                    if observation in seen_failure_attempts:
                                        continue
                                    seen_failure_attempts.add(observation)
                                    row = aggregates.setdefault((kind, identifier),
                                        {"failed_attempts": 0, "first_attempt_date": attempt["date"],
                                         "last_attempt_date": attempt["date"]})
                                    row["failed_attempts"] += 1
                                    row["first_attempt_date"] = min(row["first_attempt_date"], attempt["date"])
                                    row["last_attempt_date"] = max(row["last_attempt_date"], attempt["date"])
                        except SystemExit:
                            coverage["land_scan_complete"] = False
                        except (OSError, ValueError, TypeError, UnicodeError):
                            coverage["land_scan_complete"] = False
            except OSError:
                coverage["land_scan_complete"] = False
        unresolved_missing = missing_logs - requests_with_logs
        coverage["land_logs_missing"] = len(unresolved_missing)
        if unresolved_missing:
            coverage["land_scan_complete"] = False
        items = []
        for (kind, identifier), counts in sorted(aggregates.items()):
            if len(items) >= _LAND_FAILURE_LIMIT:
                coverage["land_scan_complete"] = False
                break
            ref = "land:" + hashlib.sha256((kind + ":" + identifier).encode()).hexdigest()[:12]
            items.append({"ref": ref, "kind": kind, "name": identifier, **counts})
        return items, coverage
    except (OSError, subprocess.SubprocessError, ValueError):
        coverage["land_scan_complete"] = False
        return [], coverage


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

    land_items, land_coverage = _collect_land_failures(repo)
    failure_section = sections["failures"]
    failure_section["items"].extend(land_items)
    failure_coverage = failure_section["coverage"]
    failure_coverage.update(land_coverage)
    failure_coverage["complete"] = bool(failure_coverage.get("complete", False) and
                                         land_coverage["land_scan_complete"])

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
        verdict_counts = {}
        for number, line in enumerate(raw.splitlines(), 1):
            if line.startswith("hypothesis:"):
                anchor = "line-%d-%s" % (number, _fingerprint(line)[:16])
                fields = line.split("|")
                verdict_field = next((field.strip()[len("verdict:"):].strip()
                                      for field in fields[1:]
                                      if field.strip().lower().startswith("verdict:")), None)
                verdict_value = (verdict_field.split()[0].lower()
                                 if verdict_field and verdict_field.split() else None)
                verdict = (verdict_value if verdict_value is not None else
                           fields[3].strip().split()[0].lower()
                           if len(fields) >= 4 and fields[3].strip().split() else "unknown")
                matches.append({"ref": anchor, "line": number, "text": line,
                                "verdict": verdict})
                verdict_counts[verdict] = verdict_counts.get(verdict, 0) + 1
        sections["rejected"] = _section(
            matches, {"complete": True, "hypotheses": len(matches),
                      "verdict_counts": verdict_counts})
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
