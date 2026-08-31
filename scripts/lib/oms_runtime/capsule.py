"""Sanitized, digest-verified, non-authoritative cross-machine continuity capsules."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Optional

from . import CAPSULE_SCHEMA, RUNTIME_SCHEMA
from .common import CoreError, atomic_write_bytes, atomic_write_json, bounded_line, canonical_json, read_json, read_text, relative_path, sanitize_portable, sensitive_text, sha256_bytes, utc_now

MAX_CAPSULE_BYTES = 1024 * 1024


def _payload(repo: Path) -> Dict[str, Any]:
    # Local import avoids a projection -> capsule -> evidence -> projection
    # cycle: imported capsules are a read-only projection input, while export
    # still needs the finalized evidence envelope.
    from .evidence import build_envelope

    envelope = build_envelope(repo)
    payload = {
        "schema": CAPSULE_SCHEMA,
        "created_at": utc_now(),
        "authority_transfer": False,
        "project": {
            "head": envelope.get("repo", {}).get("head"),
            "state_digest": envelope.get("state_digest"),
        },
        "contract": {
            "objective": envelope.get("objective"),
            "scope": envelope.get("scope"),
            "criteria": [
                {
                    "id": item.get("id"),
                    "text": item.get("text"),
                    "source": item.get("source"),
                    "status": item.get("status"),
                    "evidence_refs": [evidence.get("evidence_ref") for evidence in item.get("evidence", [])],
                }
                for item in envelope.get("criteria", [])
            ],
            "coverage": envelope.get("evidence", {}).get("coverage"),
            "complete": envelope.get("evidence", {}).get("complete"),
            "budget": envelope.get("budget"),
        },
        "continuity": {
            "next_action_ids": [item.get("id") for item in envelope.get("next_actions", [])],
            "failures": [
                {
                    "id": item.get("id"),
                    "kind": item.get("kind"),
                    "classification": item.get("classification"),
                    "summary": item.get("summary"),
                }
                for item in envelope.get("failures", [])
            ],
            "warnings": envelope.get("warnings", []),
            "task_state": envelope.get("task", {}).get("state"),
            "next_step": envelope.get("task", {}).get("next"),
        },
    }
    cleaned = sanitize_portable(payload)
    if not isinstance(cleaned, dict):
        raise CoreError("portable capsule sanitization produced an invalid payload")
    encoded = canonical_json(cleaned)
    if len(encoded) > MAX_CAPSULE_BYTES:
        raise CoreError("portable capsule exceeds %d bytes" % MAX_CAPSULE_BYTES)
    if sensitive_text(encoded.decode("utf-8")):
        raise CoreError("portable capsule still contains sensitive-looking content")
    return cleaned


def build(repo: Path) -> Dict[str, Any]:
    payload = _payload(repo)
    digest = sha256_bytes(canonical_json(payload))
    return {
        "schema": CAPSULE_SCHEMA,
        "capsule_id": "capsule-" + digest[:32],
        "digest": digest,
        "payload": payload,
    }


def export(repo: Path, output: Optional[Path] = None) -> Dict[str, Any]:
    envelope = build(repo)
    if output is None:
        output = repo / ".oms" / "portable" / "capsules" / (envelope["capsule_id"] + ".json")
    written = atomic_write_json(output, envelope)
    return {
        "schema": RUNTIME_SCHEMA,
        "capsule_id": envelope["capsule_id"],
        "digest": envelope["digest"],
        "path": relative_path(written, repo) or written.name,
        "authority_transfer": False,
    }


def validate(raw: Any) -> Dict[str, Any]:
    if not isinstance(raw, dict) or raw.get("schema") != CAPSULE_SCHEMA:
        raise CoreError("unsupported portable capsule envelope schema")
    payload = raw.get("payload")
    if not isinstance(payload, dict) or payload.get("schema") != CAPSULE_SCHEMA:
        raise CoreError("portable capsule payload is invalid")
    if payload.get("authority_transfer") is not False:
        raise CoreError("a portable capsule must not transfer authority")
    expected = str(raw.get("digest", ""))
    actual = sha256_bytes(canonical_json(payload))
    if not re.fullmatch(r"[0-9a-f]{64}", expected) or expected != actual:
        raise CoreError("portable capsule digest mismatch")
    capsule_id = str(raw.get("capsule_id", ""))
    if capsule_id != "capsule-" + expected[:32]:
        raise CoreError("portable capsule id does not match its digest")
    encoded = canonical_json(raw)
    if len(encoded) > MAX_CAPSULE_BYTES:
        raise CoreError("portable capsule exceeds %d bytes" % MAX_CAPSULE_BYTES)
    if sensitive_text(encoded.decode("utf-8")):
        raise CoreError("portable capsule contains sensitive-looking content")
    return raw


def verify(path: Path) -> Dict[str, Any]:
    raw = read_json(path, default=None, limit=MAX_CAPSULE_BYTES)
    row = validate(raw)
    return {
        "schema": RUNTIME_SCHEMA,
        "valid": True,
        "capsule_id": row["capsule_id"],
        "digest": row["digest"],
        "authority_transfer": False,
    }


def import_capsule(repo: Path, path: Path) -> Dict[str, Any]:
    raw = read_json(path, default=None, limit=MAX_CAPSULE_BYTES)
    row = validate(raw)
    destination = repo / ".oms" / "portable" / "imports" / (row["capsule_id"] + ".json")
    idempotent = False
    if destination.exists() or destination.is_symlink():
        existing = read_json(destination, default=None, limit=MAX_CAPSULE_BYTES)
        existing_capsule = existing.get("capsule") if isinstance(existing, dict) else None
        if not isinstance(existing_capsule, dict):
            raise CoreError("existing portable capsule import is invalid: %s" % destination, exit_code=75)
        try:
            validated_existing = validate(existing_capsule)
        except CoreError as exc:
            raise CoreError("existing portable capsule import is invalid: %s" % destination, exit_code=75) from exc
        if validated_existing.get("digest") != row.get("digest"):
            raise CoreError("portable capsule import conflicts with existing local state", exit_code=75)
        idempotent = True
    else:
        atomic_write_json(destination, {"schema": 1, "imported_at": utc_now(), "advisory": True, "capsule": row})
    atomic_write_bytes(destination.parent / "LATEST", (row["capsule_id"] + "\n").encode("ascii"))
    return {
        "schema": RUNTIME_SCHEMA,
        "imported": not idempotent,
        "idempotent": idempotent,
        "capsule_id": row["capsule_id"],
        "digest": row["digest"],
        "path": relative_path(destination, repo),
        "authority_transfer": False,
        "message": "imported as advisory continuity state; no task, plan, approval, lease, or publication authority changed",
    }


def latest_import(repo: Path, *, local_head: str = "") -> Dict[str, Any]:
    """Return the latest validated import as bounded, advisory-only state."""
    oms_root = repo / ".oms"
    portable_root = oms_root / "portable"
    imports_root = portable_root / "imports"
    for directory in (oms_root, portable_root, imports_root):
        if directory.is_symlink() or (directory.exists() and not directory.is_dir()):
            raise CoreError("portable capsule import directory must be a regular non-symlink directory: %s" % directory)
    if not imports_root.exists():
        return {"present": False}

    pointer = imports_root / "LATEST"
    if not pointer.exists() and not pointer.is_symlink():
        return {"present": False}
    capsule_id = read_text(pointer, limit=128)
    if not re.fullmatch(r"capsule-[0-9a-f]{32}\n", capsule_id):
        raise CoreError("portable capsule LATEST pointer is invalid")
    capsule_id = capsule_id.rstrip("\n")

    path = imports_root / (capsule_id + ".json")
    wrapper = read_json(path, default=None, limit=MAX_CAPSULE_BYTES)
    if (not isinstance(wrapper, dict) or wrapper.get("schema") != 1 or
            wrapper.get("advisory") is not True or
            not isinstance(wrapper.get("capsule"), dict)):
        raise CoreError("latest portable capsule import wrapper is invalid")
    row = validate(wrapper["capsule"])
    if row.get("capsule_id") != capsule_id:
        raise CoreError("portable capsule LATEST pointer does not match its import")

    project = row.get("payload", {}).get("project", {})
    source_head = str(project.get("head") or "") if isinstance(project, dict) else ""
    source_state_digest = str(project.get("state_digest") or "") if isinstance(project, dict) else ""
    if not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", source_head):
        source_head = ""
    if not re.fullmatch(r"[0-9a-f]{64}", source_state_digest):
        source_state_digest = ""
    head_matches = source_head == local_head if source_head and local_head else None
    return {
        "present": True,
        "advisory": True,
        "authority_transfer": False,
        "capsule_id": capsule_id,
        "digest": row["digest"],
        "imported_at": bounded_line(wrapper.get("imported_at", ""), 40),
        "path": relative_path(path, repo),
        "source_head": source_head or None,
        "source_state_digest": source_state_digest or None,
        "head_matches": head_matches,
        "state_digest_matches": None,
        "status": "head-diverged" if head_matches is False else "unknown",
    }


def diff(left: Path, right: Path) -> Dict[str, Any]:
    a = validate(read_json(left, default=None, limit=MAX_CAPSULE_BYTES))
    b = validate(read_json(right, default=None, limit=MAX_CAPSULE_BYTES))
    left_criteria = {item.get("id"): item for item in a["payload"].get("contract", {}).get("criteria", [])}
    right_criteria = {item.get("id"): item for item in b["payload"].get("contract", {}).get("criteria", [])}
    changes: List[Dict[str, Any]] = []
    for criterion_id in sorted(set(left_criteria) | set(right_criteria)):
        before = left_criteria.get(criterion_id)
        after = right_criteria.get(criterion_id)
        if before != after:
            changes.append({"criterion_id": criterion_id, "before": before, "after": after})
    return {
        "schema": RUNTIME_SCHEMA,
        "left": a["digest"],
        "right": b["digest"],
        "head_changed": a["payload"].get("project", {}).get("head") != b["payload"].get("project", {}).get("head"),
        "state_changed": a["payload"].get("project", {}).get("state_digest") != b["payload"].get("project", {}).get("state_digest"),
        "criterion_changes": changes,
    }
