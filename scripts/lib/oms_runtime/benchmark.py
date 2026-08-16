"""Content-free harness effectiveness telemetry and comparisons."""

from __future__ import annotations

import collections
import statistics
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence

from . import RUNTIME_SCHEMA
from .common import MAX_JSONL_ROWS, CoreError, append_jsonl, bounded_line, read_json, read_jsonl, safe_id, sha256_text, utc_now
from .evidence import build_envelope, outcome


def _find_numbers(value: Any, keys: Sequence[str]) -> List[float]:
    result: List[float] = []
    wanted = set(keys)
    if isinstance(value, dict):
        for key, item in value.items():
            if key in wanted and isinstance(item, (int, float)) and not isinstance(item, bool):
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
    input_tokens = _one_number(row, ("input_tokens", "prompt_tokens"))
    output_tokens = _one_number(row, ("output_tokens", "completion_tokens"))
    if input_tokens is None and output_tokens is None:
        return _one_number(row, ("tokens",))
    return float(input_tokens or 0.0) + float(output_tokens or 0.0)


def _context_rows(repo: Path) -> List[Dict[str, Any]]:
    root = repo / ".oms" / "runtime" / "context"
    rows: List[Dict[str, Any]] = []
    if not root.is_dir() or root.is_symlink():
        return rows
    for path in sorted(root.glob("*.json"))[-1000:]:
        row = read_json(path, default=None)
        if isinstance(row, dict):
            rows.append(row)
    return rows


def outcome_path(repo: Path) -> Path:
    return repo / ".oms" / "runtime" / "outcomes.jsonl"


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
    envelope = build_envelope(repo)
    outcomes = [outcome(row) for row in artifacts]
    decided = [value for value in outcomes if value in ("verified", "failed")]
    verified = sum(value == "verified" for value in decided)
    providers = collections.Counter(str(row.get("provider")) for row in artifacts if row.get("provider"))
    kinds = collections.Counter(str(row.get("kind", "unknown")) for row in artifacts)
    artifact_durations = [value for row in artifacts for value in [_one_number(row, ("duration_seconds", "duration_s"))] if value is not None]
    lifecycle_durations = [value for row in events for value in [_one_number(row, ("duration_seconds", "duration_s"))] if value is not None]
    durations = artifact_durations or lifecycle_durations
    tokens = [value for row in artifacts for value in [_artifact_tokens(row)] if value is not None]
    costs = [value for row in artifacts for value in [_one_number(row, ("cost_usd", "cost"))] if value is not None]
    context_bytes = [float(row.get("selected_bytes")) for row in contexts if isinstance(row.get("selected_bytes"), (int, float))]
    context_debt = [float(row.get("context_debt")) for row in contexts if isinstance(row.get("context_debt"), (int, float))]
    acceptance_weight = sum(float(item.get("weight", 1)) for item in envelope.get("criteria", []) if item.get("status") == "verified")
    denominator = (sum(tokens) if tokens else 0.0) + 100.0 * len(artifacts) + (sum(durations) if durations else 0.0)
    efficiency = acceptance_weight / denominator if denominator > 0 else None
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
        "generated_at": utc_now(),
        "state_digest": envelope.get("state_digest"),
        "acceptance": {"coverage": envelope.get("evidence", {}).get("coverage"), "risk_score": envelope.get("evidence", {}).get("risk_score"), "complete": envelope.get("evidence", {}).get("complete"), "verified_weight": acceptance_weight},
        "artifacts": len(artifacts),
        "lifecycle_events": len(events),
        "decided_outcomes": len(decided),
        "verified_outcomes": verified,
        "success_rate": verified / len(decided) if decided else None,
        "providers": dict(sorted(providers.items())),
        "artifact_kinds": dict(sorted(kinds.items())),
        "duration_seconds": {"count": len(durations), "sum": sum(durations) if durations else None, "mean": statistics.mean(durations) if durations else None},
        "tokens": {"count": len(tokens), "sum": sum(tokens) if tokens else None},
        "cost_usd": {"count": len(costs), "sum": sum(costs) if costs else None},
        "context": {"manifests": len(contexts), "selected_bytes_sum": sum(context_bytes) if context_bytes else None, "selected_bytes_mean": statistics.mean(context_bytes) if context_bytes else None, "debt_sum": sum(context_debt) if context_debt else None},
        "useful_work_efficiency": efficiency,
        "manual_outcomes": {"count": len(manual_outcomes), "totals": manual_metrics},
        "unknown_metrics": [] if manual_outcomes else ["human_corrections", "escaped_defects", "reverted_lines", "false_refusals", "duplicate_work"],
    }


def persist(repo: Path, row: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    value = dict(row or snapshot(repo))
    append_jsonl(repo / ".oms" / "runtime" / "effectiveness.jsonl", value)
    return value


def compare(left: Mapping[str, Any], right: Mapping[str, Any]) -> Dict[str, Any]:
    fields = [("success_rate",), ("useful_work_efficiency",), ("acceptance", "coverage"), ("acceptance", "risk_score"), ("context", "selected_bytes_mean"), ("tokens", "sum"), ("cost_usd", "sum"), ("duration_seconds", "sum")]

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
        changes.append({"field": ".".join(path), "left": a, "right": b, "delta": delta})
    return {"schema": RUNTIME_SCHEMA, "changes": changes}
