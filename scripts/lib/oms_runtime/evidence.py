"""Criterion-linked evidence projection over existing OMS receipts."""

from __future__ import annotations

import collections
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence

from . import RUNTIME_SCHEMA
from .common import MAX_JSONL_ROWS, CoreError, append_jsonl, bounded_line, git_head, parse_path_list, read_jsonl, relative_path, safe_id, sha256_file, utc_now
from .projection import build_base_envelope, finalize_envelope

VALID_STATUSES = {"verified", "failed", "inconclusive", "skipped_with_reason", "stale"}


def _artifact_paths(repo: Path) -> List[Path]:
    candidates = [repo / ".oms" / "artifacts" / "index.jsonl", repo / ".oms" / "plan" / "progress.jsonl", repo / ".oms" / "lifecycle" / "events.jsonl"]
    return [path for path in candidates if path.is_file() and not path.is_symlink()]


def artifact_rows(repo: Path) -> List[Dict[str, Any]]:
    result: List[Dict[str, Any]] = []
    ordinal = 0
    for path in _artifact_paths(repo):
        for row in read_jsonl(path, limit_rows=MAX_JSONL_ROWS):
            ordinal += 1
            copy = dict(row)
            copy["_source"] = relative_path(path, repo)
            copy["_ordinal"] = ordinal
            result.append(copy)
    return result


def evidence_ref(row: Mapping[str, Any]) -> str:
    for key in ("event_id", "id", "operation_id", "attempt_id", "run_id", "delegation_id"):
        value = row.get(key)
        if value:
            return bounded_line(value, 160)
    return ""


def criterion_refs(row: Mapping[str, Any]) -> List[str]:
    refs: List[str] = []
    for key in ("covers", "criteria", "criterion_ids", "acceptance_criteria"):
        value = row.get(key)
        if isinstance(value, str):
            refs.extend(item for item in value.replace(",", " ").split() if item)
        elif isinstance(value, list):
            refs.extend(str(item).strip() for item in value if str(item).strip())
    nested = row.get("evidence")
    if isinstance(nested, dict):
        refs.extend(criterion_refs(nested))
    return sorted(set(refs))


def outcome(row: Mapping[str, Any]) -> Optional[str]:
    status = str(row.get("status", row.get("state", row.get("verdict", "")))).lower()
    if status in ("pass", "passed", "success", "succeeded", "verified", "done", "approved", "admit", "admitted"):
        return "verified"
    if status in ("fail", "failed", "error", "rejected", "reject", "blocked", "denied"):
        return "failed"
    if status in ("inconclusive", "unknown", "partial"):
        return "inconclusive"
    if status in ("skipped", "skipped_with_reason"):
        return "skipped_with_reason"
    review = row.get("review")
    if isinstance(review, dict):
        nested = outcome(review)
        if nested:
            return nested
    for key in ("verify_exit", "exit", "exit_code"):
        value = row.get(key)
        if isinstance(value, bool):
            continue
        if isinstance(value, int):
            return "verified" if value == 0 else "failed"
        if isinstance(value, str) and value.lstrip("-").isdigit():
            return "verified" if int(value) == 0 else "failed"
    return None


def _dependency_digests(repo: Path, paths: Sequence[str]) -> Dict[str, str]:
    result: Dict[str, str] = {}
    for raw in paths:
        path = repo / raw
        if not path.is_file() or path.is_symlink():
            raise CoreError("evidence dependency is not a regular file: %s" % raw)
        result[raw] = sha256_file(path)
    return result


def _stale(row: Mapping[str, Any], repo: Path) -> bool:
    dependencies = row.get("dependency_digests", row.get("dependencies"))
    if isinstance(dependencies, dict) and dependencies:
        for raw, expected in dependencies.items():
            path = repo / str(raw)
            if not path.is_file() or path.is_symlink() or not isinstance(expected, str) or sha256_file(path) != expected:
                return True
        return False
    verified_head = row.get("verified_head", row.get("evidence_head"))
    if not verified_head and str(row.get("kind", "")).lower() == "acceptance":
        verified_head = row.get("base_sha")
    current = git_head(repo)
    return bool(isinstance(verified_head, str) and verified_head and current and verified_head != current)


def _bindings(repo: Path) -> List[Dict[str, Any]]:
    path = repo / ".oms" / "evidence" / "bindings.jsonl"
    return read_jsonl(path, limit_rows=MAX_JSONL_ROWS) if path.is_file() else []


def _active_bindings(repo: Path) -> List[Dict[str, Any]]:
    rows = _bindings(repo)
    revoked = {str(row.get("revokes", "")) for row in rows if row.get("action") == "revoke"}
    return [row for row in rows if row.get("action", "bind") == "bind" and str(row.get("binding_id", "")) and str(row.get("binding_id", "")) not in revoked]


def revoke(repo: Path, binding_id: str, *, reason: str = "") -> Dict[str, Any]:
    binding_id = safe_id(binding_id, "binding id")
    active = {str(row.get("binding_id")): row for row in _active_bindings(repo)}
    if binding_id not in active:
        raise CoreError("active evidence binding not found: %s" % binding_id)
    row = {"schema": 1, "binding_id": "revocation-%s" % __import__("uuid").uuid4().hex, "action": "revoke", "created_at": utc_now(), "revokes": binding_id, "criterion_id": active[binding_id].get("criterion_id"), "evidence_ref": active[binding_id].get("evidence_ref"), "reason": bounded_line(reason or "binding no longer applies", 300)}
    append_jsonl(repo / ".oms" / "evidence" / "bindings.jsonl", row)
    return row


def bind(repo: Path, criterion_id: str, ref: str, status: str, *, evidence_type: str = "explicit-binding", note: str = "", dependencies: Sequence[str] = ()) -> Dict[str, Any]:
    criterion_id = safe_id(criterion_id, "criterion id")
    ref = safe_id(ref, "evidence ref")
    if status not in VALID_STATUSES:
        raise CoreError("unsupported evidence status: %s" % status)
    base = build_base_envelope(repo)
    criterion_ids = {str(item.get("id")) for item in base.get("criteria", [])}
    if criterion_id not in criterion_ids:
        raise CoreError("unknown current acceptance criterion: %s" % criterion_id)
    existing = {evidence_ref(row) for row in artifact_rows(repo)}
    task_ref = "task-verification:%s" % base.get("task", {}).get("task_id", "")
    if ref not in existing and ref != task_ref:
        raise CoreError("evidence reference does not exist in current OMS receipts: %s" % ref)
    if ref == task_ref and base.get("task", {}).get("verification") != "fresh":
        raise CoreError("task verification is not fresh and cannot be bound")
    dependency_digests = _dependency_digests(repo, parse_path_list(list(dependencies)))
    row = {"schema": 1, "binding_id": "binding-%s" % __import__("uuid").uuid4().hex, "action": "bind", "created_at": utc_now(), "criterion_id": criterion_id, "evidence_ref": ref, "status": status, "evidence_type": bounded_line(evidence_type, 80), "note": bounded_line(note, 300), "verified_head": None if dependency_digests else (git_head(repo) or None), "dependency_digests": dependency_digests}
    append_jsonl(repo / ".oms" / "evidence" / "bindings.jsonl", row)
    return row


def _evidence_item(row: Mapping[str, Any], repo: Path, *, support: str) -> Dict[str, Any]:
    status = outcome(row) or str(row.get("status", "inconclusive"))
    stale = _stale(row, repo)
    if stale:
        status = "stale"
    return {"evidence_ref": evidence_ref(row) or bounded_line(row.get("binding_id", ""), 160), "binding_id": bounded_line(row.get("binding_id", ""), 160), "kind": bounded_line(row.get("kind", row.get("evidence_type", "artifact")), 80), "provider": bounded_line(row.get("provider", ""), 80), "status": status, "stale": stale, "ts": bounded_line(row.get("ts", row.get("created_at", "")), 40), "scope_digest": bounded_line(row.get("scope_digest", row.get("reviewed_diff_sha256", "")), 80), "support": support, "source": bounded_line(row.get("_source", ""), 200), "_ordinal": int(row.get("_ordinal", 0)) if isinstance(row.get("_ordinal", 0), int) else 0}


def _item_sort_key(item: Mapping[str, Any]) -> tuple:
    ordinal = item.get("_ordinal", 0)
    return (str(item.get("ts", "") or ""), int(ordinal) if isinstance(ordinal, int) else 0)


def _project_status(items: Sequence[Mapping[str, Any]]) -> str:
    if not items:
        return "missing"
    ordered = sorted(items, key=_item_sort_key)
    fresh = [item for item in ordered if str(item.get("status")) != "stale"]
    if not fresh:
        return "stale"
    latest_key = _item_sort_key(fresh[-1])
    statuses = [str(item.get("status")) for item in fresh if _item_sort_key(item) == latest_key]
    for candidate in ("failed", "verified", "inconclusive", "skipped_with_reason"):
        if candidate in statuses:
            return candidate
    return "inconclusive"


def build_coverage(repo: Path, base: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    base_envelope = dict(base or build_base_envelope(repo))
    criteria = list(base_envelope.get("criteria", []))
    valid_ids = {str(item.get("id")) for item in criteria}
    by_criterion: Dict[str, List[Dict[str, Any]]] = collections.defaultdict(list)
    unbound: List[Dict[str, Any]] = []
    rows = artifact_rows(repo)
    row_by_ref = {evidence_ref(row): row for row in rows if evidence_ref(row)}
    plan_criteria = {str(item.get("id")): str(item.get("command_digest", "")) for item in criteria if item.get("source") == "plan" and item.get("command_digest")}
    for row in rows:
        refs = [ref for ref in criterion_refs(row) if ref in valid_ids]
        support = "explicit-artifact"
        if not refs and str(row.get("kind", "")).lower() == "acceptance":
            observed = str(row.get("accept_sha256", ""))
            if observed:
                refs = [criterion_id for criterion_id, digest in plan_criteria.items() if digest.startswith(observed)]
                if refs:
                    support = "plan-acceptance-receipt"
        item = _evidence_item(row, repo, support=support)
        if refs:
            for ref in refs:
                by_criterion[ref].append(item)
        elif outcome(row) is not None:
            unbound.append(item)
    active_bindings = _active_bindings(repo)
    for binding_index, binding in enumerate(active_bindings, len(rows) + 1):
        binding = dict(binding)
        binding["_ordinal"] = binding_index
        criterion_id = str(binding.get("criterion_id", ""))
        if criterion_id not in valid_ids:
            continue
        merged = dict(row_by_ref.get(str(binding.get("evidence_ref", "")), {}))
        merged.update(binding)
        by_criterion[criterion_id].append(_evidence_item(merged, repo, support="explicit-binding"))
    task = base_envelope.get("task", {})
    if task.get("verification") == "fresh":
        task_ref = "task-verification:%s" % task.get("task_id", "")
        for criterion in criteria:
            if criterion.get("source") == "task" and not by_criterion.get(str(criterion.get("id"))):
                by_criterion[str(criterion.get("id"))].append({"evidence_ref": task_ref, "kind": "task-verification", "provider": "", "status": "verified", "stale": False, "ts": bounded_line(task.get("metadata", {}).get("updated", task.get("metadata", {}).get("last_activity", "")), 40) if isinstance(task.get("metadata"), dict) else "", "scope_digest": task.get("verify_digest", ""), "support": "fresh-task-gate", "source": ".oms/task/current.md", "_ordinal": len(rows) + len(active_bindings) + 1})
    projected: List[Dict[str, Any]] = []
    counts: collections.Counter = collections.Counter()
    for criterion in criteria:
        criterion_id = str(criterion.get("id"))
        items = sorted(by_criterion.get(criterion_id, []), key=_item_sort_key)[-20:]
        status = _project_status(items)
        counts[status] += 1
        projected.append(dict(criterion, status=status, evidence=[{key: value for key, value in item.items() if not key.startswith("_")} for item in items]))
    total_weight = sum(float(item.get("weight", 1)) for item in projected)
    verified_weight = sum(float(item.get("weight", 1)) for item in projected if item.get("status") == "verified")
    risk_weight = sum(float(item.get("weight", 1)) * {"failed": 1.0, "missing": 0.8, "stale": 0.7, "inconclusive": 0.5, "skipped_with_reason": 0.4}.get(str(item.get("status")), 0.0) for item in projected)
    return {"schema": RUNTIME_SCHEMA, "criteria": projected, "counts": dict(sorted(counts.items())), "coverage": verified_weight / total_weight if total_weight else 0.0, "risk_score": risk_weight / total_weight if total_weight else 1.0, "complete": bool(projected) and all(item.get("status") == "verified" for item in projected), "unbound_evidence": unbound[-50:], "artifact_rows_seen": len(rows)}


def build_envelope(repo: Path) -> Dict[str, Any]:
    base = build_base_envelope(repo)
    return finalize_envelope(base, build_coverage(repo, base))
