"""Content-free harness effectiveness telemetry and comparisons."""

from __future__ import annotations

import collections
import datetime as dt
import heapq
import math
import re
import statistics
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from . import RUNTIME_SCHEMA
from .common import MAX_JSONL_ROWS, CoreError, append_jsonl, bounded_line, read_json, read_jsonl, safe_id, sha256_text, utc_now
from .evidence import build_envelope, outcome


def _find_numbers(value: Any, keys: Sequence[str]) -> List[float]:
    result: List[float] = []
    wanted = set(keys)
    if isinstance(value, dict):
        for key, item in value.items():
            if key in wanted and isinstance(item, (int, float)) and not isinstance(item, bool) and math.isfinite(item) and item >= 0:
                result.append(float(item))
            result.extend(_find_numbers(item, keys))
    elif isinstance(value, list):
        for item in value:
            result.extend(_find_numbers(item, keys))
    return result


def _one_number(value: Any, keys: Sequence[str]) -> Optional[float]:
    found = _find_numbers(value, keys)
    return found[0] if found else None


def _artifact_tokens(row: Mapping[str, Any]) -> Optional[float]:
    total = _one_number(row, ("total_tokens",))
    if total is not None:
        return total
    usage = row.get("provider_usage")
    if isinstance(usage, dict):
        # Codex includes cached input; Claude reports it separately. The
        # historical tokens footer omits Claude cache and can be partial.
        keys = ["input_tokens", "output_tokens"]
        if usage.get("cache_in_input") is False:
            keys.extend(("cache_read_tokens", "cache_write_tokens"))
        elif usage.get("cache_in_input") is not True:
            return None
        values = [_one_number(usage, (key,)) for key in keys]
        return sum(values) if all(value is not None for value in values) else None
    input_tokens = _one_number(row, ("input_tokens", "prompt_tokens"))
    output_tokens = _one_number(row, ("output_tokens", "completion_tokens"))
    if input_tokens is None and output_tokens is None:
        return _one_number(row, ("tokens",))
    return input_tokens + output_tokens if input_tokens is not None and output_tokens is not None else None


def _model_identity(row: Mapping[str, Any]) -> Tuple[Optional[str], Optional[str]]:
    # A fallback row combines more than one attempt's outcome, duration, and
    # usage. It stays in operation-level totals but cannot honestly vote in a
    # per-model table until attempts have separate receipts.
    if row.get("fallback_used") is True or row.get("model_attribution") == "ambiguous":
        return None, None
    for key, source in (
        ("served_model", "transport"),
        ("configured_model", "configured-default"),
        ("selected_model", "selected"),
        ("requested_model", "selected"),
    ):
        value = row.get(key)
        if isinstance(value, str) and value and value != "provider-default":
            return value, source
    return None, None


def _provider_call(row: Mapping[str, Any]) -> bool:
    return row.get("kind") in ("call", "ask", "review", "delegate") or (
        row.get("kind") == "review-synthesis" and row.get("provider") not in (None, "", "local"))


def _verification_outcome(row: Mapping[str, Any]) -> Optional[str]:
    if row.get("kind") == "delegate":
        return outcome(row) if row.get("context_verification") == "command" else None
    if row.get("kind") in ("review-verify", "acceptance", "task-verification"):
        return outcome(row)
    return None


def _completion_outcome(row: Mapping[str, Any]) -> Optional[str]:
    if row.get("context_verification") == "dry-run":
        return None
    return outcome({key: row[key] for key in ("status", "exit", "exit_code") if key in row})


def _outcome_counts(values: Sequence[Optional[str]]) -> Dict[str, Any]:
    passed, failed = values.count("verified"), values.count("failed")
    decided = passed + failed
    return {"count": len(values), "passed": passed, "failed": failed,
            "unknown": len(values) - decided, "success_rate": passed / decided if decided else None}


def _model_table(artifacts: Sequence[Mapping[str, Any]]) -> Dict[str, Dict[str, Any]]:
    table: Dict[str, Dict[str, Any]] = {}
    durations: Dict[str, List[float]] = {}
    completions: Dict[str, List[Optional[str]]] = collections.defaultdict(list)
    for row in artifacts:
        if not _provider_call(row):
            continue
        name, source = _model_identity(row)
        if not name:
            continue
        key = "%s/%s" % (row.get("provider") or "unknown", name)
        entry = table.setdefault(key, {"calls": 0, "verified": 0, "failed": 0, "tokens": None, "cost_usd": None, "tokens_count": 0, "cost_usd_count": 0, "sources": {}})
        entry["calls"] += 1
        entry["sources"][source] = entry["sources"].get(source, 0) + 1
        completions[key].append(_completion_outcome(row))
        value = _verification_outcome(row)
        if value == "verified":
            entry["verified"] += 1
        elif value == "failed":
            entry["failed"] += 1
        tokens = _artifact_tokens(row)
        if tokens is not None:
            entry["tokens"] = (entry["tokens"] or 0.0) + tokens
            entry["tokens_count"] += 1
        cost = _one_number(row, ("cost_usd", "cost"))
        if cost is not None:
            entry["cost_usd"] = (entry["cost_usd"] or 0.0) + cost
            entry["cost_usd_count"] += 1
        duration = _one_number(row, ("duration_seconds", "duration_s"))
        if duration is not None:
            durations.setdefault(key, []).append(duration)
    for key, entry in table.items():
        decided = entry["verified"] + entry["failed"]
        entry["success_rate"] = entry["verified"] / decided if decided else None
        entry["completion"] = _outcome_counts(completions[key])
        samples = durations.get(key, [])
        entry["duration_seconds_mean"] = statistics.mean(samples) if samples else None
        entry["duration_seconds_count"] = len(samples)
        entry["sources"] = dict(sorted(entry["sources"].items()))
    return dict(sorted(table.items()))


def _context_modes(artifacts: Sequence[Mapping[str, Any]]) -> Dict[str, Any]:
    modes: Dict[str, List[Mapping[str, Any]]] = collections.defaultdict(list)
    cohorts: Dict[str, Dict[str, List[Mapping[str, Any]]]] = {}
    fields = ("context_prepare_seconds", "context_orientation_bytes", "prompt_bytes", "duration_seconds")

    def summarize(rows):
        checks = [row for row in rows if row.get("context_verification") == "command"
                  and type(row.get("verify_exit")) is int and 0 <= row["verify_exit"] < 125]
        verified = sum(outcome(row) == "verified" for row in checks)
        checked = len(checks)
        result = {"calls": len(rows), "verified": verified, "checked": checked,
                  "verification_rate": verified / checked if checked else None}
        for field in fields + ("tokens", "cost_usd"):
            keys = ("duration_seconds", "duration_s") if field == "duration_seconds" else (field,)
            samples = [_artifact_tokens(row) if field == "tokens" else _one_number(row, keys) for row in rows]
            samples = [value for value in samples if value is not None]
            result[field] = {"count": len(samples), "mean": statistics.mean(samples) if samples else None}
        return result

    for row in artifacts:
        mode = row.get("context_mode")
        if row.get("kind") != "delegate" or mode not in {"direct", "pack", "graph", "graph-fallback", "bundle", "pack+bundle", "graph+bundle", "graph-fallback+bundle"}:
            continue
        modes[mode].append(row)
        model, source = _model_identity(row)
        task = row.get("context_task_sha256", "")
        base = row.get("base_sha", "")
        # Aggregate modes are descriptive. Only exact task/verifier/cap/base
        # and attributable model/effort matches enter a comparison cohort.
        if not model or row.get("context_verification") != "command" or not isinstance(task, str) or not re.fullmatch(r"[0-9a-f]{64}", task) or not base:
            continue
        key = sha256_text(str((base, row.get("provider"), model, source,
                              row.get("selected_reasoning_effort", row.get("reasoning_effort")), task)))
        cohorts.setdefault(key, {}).setdefault(mode, []).append(row)
    return {"modes": {mode: summarize(rows) for mode, rows in sorted(modes.items())},
            "matched_cohorts": {key: {mode: summarize(rows) for mode, rows in sorted(groups.items())}
                                for key, groups in sorted(cohorts.items()) if len(groups) > 1},
            "comparison": "observational; matched cohorts do not prove causal savings"}


def _context_rows(repo: Path) -> List[Dict[str, Any]]:
    root = repo / ".oms" / "runtime" / "context"
    if not root.is_dir() or root.is_symlink():
        return []

    def candidates():
        for path in root.glob("*.json"):
            row = read_json(path, default=None)
            if not isinstance(row, dict):
                continue
            # Hash names and copied-file mtimes are not creation timestamps.
            stamp = dt.datetime.min.replace(tzinfo=dt.timezone.utc)
            try:
                parsed = dt.datetime.fromisoformat(row.get("generated_at", "").replace("Z", "+00:00"))
                if parsed.tzinfo is not None:
                    stamp = parsed.astimezone(dt.timezone.utc)
            except (AttributeError, TypeError, ValueError, OverflowError):
                pass
            yield stamp, path.name, row

    # Read all timestamps, but retain at most the sample plus one candidate.
    return [item[2] for item in heapq.nlargest(1000, candidates(), key=lambda item: item[:2])]


def outcome_path(repo: Path) -> Path:
    return repo / ".oms" / "runtime" / "outcomes.jsonl"


def skill_eval_path(repo: Path) -> Path:
    return repo / ".oms" / "runtime" / "skill-evals.jsonl"


def _skill_eval_summary(rows: Sequence[Mapping[str, Any]]) -> Dict[str, int]:
    summary = {
        "count": 0,
        "task_pass_delta_sum": 0,
        "treatment_task_passed": 0,
        "trigger_false_negatives": 0,
        "trigger_false_positives": 0,
        "trigger_true_positives": 0,
    }
    for number, row in enumerate(rows, 1):
        if row.get("schema") != 1 or not isinstance(row.get("skill"), str):
            raise CoreError("skill evaluation row %d has an invalid schema" % number)
        trigger = row.get("trigger")
        task = row.get("task")
        treatment = trigger.get("treatment") if isinstance(trigger, dict) else None
        if not isinstance(treatment, dict) or not isinstance(task, dict):
            raise CoreError("skill evaluation row %d has invalid metrics" % number)
        values = {
            "task_pass_delta_sum": task.get("pass_delta"),
            "treatment_task_passed": task.get("treatment_passed"),
            "trigger_false_negatives": treatment.get("false_negative"),
            "trigger_false_positives": treatment.get("false_positive"),
            "trigger_true_positives": treatment.get("true_positive"),
        }
        if any(isinstance(item, bool) or not isinstance(item, int) for item in values.values()):
            raise CoreError("skill evaluation row %d has non-integer metrics" % number)
        summary["count"] += 1
        for key, value in values.items():
            summary[key] += value
    return summary


def record_outcome(repo: Path, *, task_id: str, status: str, human_corrections: int = 0, escaped_defects: int = 0, reverted_lines: int = 0, false_refusals: int = 0, duplicate_work: int = 0, note: str = "") -> Dict[str, Any]:
    task_id = safe_id(task_id, "task id")
    if status not in ("verified", "failed", "partial", "blocked"):
        raise CoreError("unsupported effectiveness outcome status: %s" % status)
    metrics = {"human_corrections": human_corrections, "escaped_defects": escaped_defects, "reverted_lines": reverted_lines, "false_refusals": false_refusals, "duplicate_work": duplicate_work}
    for name, value in metrics.items():
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise CoreError("%s must be a non-negative integer" % name)
    row = {"schema": 1, "created_at": utc_now(), "task_id": task_id, "status": status, "metrics": metrics, "note_digest": sha256_text(bounded_line(note, 300)) if note else ""}
    append_jsonl(outcome_path(repo), row)
    return row


def snapshot(repo: Path) -> Dict[str, Any]:
    artifacts = read_jsonl(repo / ".oms" / "artifacts" / "index.jsonl", limit_rows=MAX_JSONL_ROWS) if (repo / ".oms" / "artifacts" / "index.jsonl").is_file() else []
    events = read_jsonl(repo / ".oms" / "lifecycle" / "events.jsonl", limit_rows=MAX_JSONL_ROWS) if (repo / ".oms" / "lifecycle" / "events.jsonl").is_file() else []
    contexts = _context_rows(repo)
    manual_outcomes = read_jsonl(outcome_path(repo), limit_rows=MAX_JSONL_ROWS) if outcome_path(repo).is_file() else []
    skill_evals = read_jsonl(skill_eval_path(repo), limit_rows=MAX_JSONL_ROWS) if skill_eval_path(repo).is_file() else []
    envelope = build_envelope(repo)
    calls = [row for row in artifacts if _provider_call(row)]
    outcomes = [_verification_outcome(row) for row in artifacts]
    decided = [value for value in outcomes if value in ("verified", "failed")]
    verified = sum(value == "verified" for value in decided)
    providers = collections.Counter(str(row.get("provider")) for row in artifacts if row.get("provider"))
    kinds = collections.Counter(str(row.get("kind", "unknown")) for row in artifacts)
    artifact_durations = [value for row in calls for value in [_one_number(row, ("duration_seconds", "duration_s"))] if value is not None]
    lifecycle_durations = [value for row in events for value in [_one_number(row, ("duration_seconds", "duration_s"))] if value is not None]
    durations = artifact_durations or lifecycle_durations
    tokens = [value for row in calls for value in [_artifact_tokens(row)] if value is not None]
    costs = [value for row in calls for value in [_one_number(row, ("cost_usd", "cost"))] if value is not None]
    context_bytes = [float(row.get("selected_bytes")) for row in contexts if isinstance(row.get("selected_bytes"), (int, float))]
    context_debt = [float(row.get("context_debt")) for row in contexts if isinstance(row.get("context_debt"), (int, float))]
    acceptance_weight = sum(float(item.get("weight", 1)) for item in envelope.get("criteria", []) if item.get("status") == "verified")
    manual_metrics: Dict[str, int] = {"human_corrections": 0, "escaped_defects": 0, "reverted_lines": 0, "false_refusals": 0, "duplicate_work": 0}
    for row in manual_outcomes:
        metrics = row.get("metrics")
        if not isinstance(metrics, dict):
            continue
        for key in manual_metrics:
            value = metrics.get(key)
            if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
                manual_metrics[key] += value
    return {
        "schema": RUNTIME_SCHEMA,
        "measurement_basis": "provider-calls/explicit-verification-v1",
        "generated_at": utc_now(),
        "state_digest": envelope.get("state_digest"),
        "acceptance": {"coverage": envelope.get("evidence", {}).get("coverage"), "risk_score": envelope.get("evidence", {}).get("risk_score"), "complete": envelope.get("evidence", {}).get("complete"), "verified_weight": acceptance_weight},
        "artifacts": len(artifacts),
        "lifecycle_events": len(events),
        "decided_outcomes": len(decided),
        "verified_outcomes": verified,
        "success_rate": verified / len(decided) if decided else None,
        "call_outcomes": _outcome_counts([_completion_outcome(row) for row in calls]),
        "review_outcomes": _outcome_counts([outcome(row) for row in artifacts if row.get("kind") == "review-outcome"]),
        "providers": dict(sorted(providers.items())),
        "models": _model_table(artifacts),
        "delegation_context": _context_modes(artifacts),
        "artifact_kinds": dict(sorted(kinds.items())),
        "duration_seconds": {"count": len(durations), "sum": sum(durations) if durations else None, "mean": statistics.mean(durations) if durations else None},
        "tokens": {"count": len(tokens), "sum": sum(tokens) if tokens else None},
        "cost_usd": {"count": len(costs), "sum": sum(costs) if costs else None},
        "context": {"manifests": len(contexts), "sample_limit": 1000, "selection": "generated_at_desc_undated_last", "selected_bytes_sum": sum(context_bytes) if context_bytes else None, "selected_bytes_mean": statistics.mean(context_bytes) if context_bytes else None, "debt_sum": sum(context_debt) if context_debt else None},
        # Legacy field: adding tokens, seconds and event counts is not a
        # calibrated efficiency measure. Keep raw measurements, not a score.
        "useful_work_efficiency": None,
        "manual_outcomes": {"count": len(manual_outcomes), "totals": manual_metrics},
        "skill_evals": _skill_eval_summary(skill_evals),
        "unknown_metrics": [] if manual_outcomes else ["human_corrections", "escaped_defects", "reverted_lines", "false_refusals", "duplicate_work"],
    }


def persist(repo: Path, row: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    value = dict(row or snapshot(repo))
    append_jsonl(repo / ".oms" / "runtime" / "effectiveness.jsonl", value)
    return value


def compare(left: Mapping[str, Any], right: Mapping[str, Any]) -> Dict[str, Any]:
    fields = [("success_rate",), ("acceptance", "coverage"), ("acceptance", "risk_score"), ("context", "selected_bytes_mean"), ("tokens", "sum"), ("cost_usd", "sum"), ("duration_seconds", "sum"), ("skill_evals", "task_pass_delta_sum")]
    fields.extend(("manual_outcomes", "totals", name) for name in (
        "human_corrections", "escaped_defects", "reverted_lines",
        "false_refusals", "duplicate_work"))

    def get(row: Mapping[str, Any], path: Sequence[str]) -> Any:
        value: Any = row
        for key in path:
            if not isinstance(value, Mapping):
                return None
            value = value.get(key)
        return value

    changes: List[Dict[str, Any]] = []
    for path in fields:
        a = get(left, path)
        b = get(right, path)
        delta = b - a if isinstance(a, (int, float)) and isinstance(b, (int, float)) else None
        item = {"field": ".".join(path), "left": a, "right": b, "delta": delta}
        if path[0] in ("success_rate", "tokens", "cost_usd", "duration_seconds") and left.get("measurement_basis") != right.get("measurement_basis"):
            item.update(delta=None, reason="measurement basis differs; not comparable")
        changes.append(item)
    return {"schema": RUNTIME_SCHEMA, "changes": changes}
