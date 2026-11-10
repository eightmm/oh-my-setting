"""Bounded, read-only inspection of recorded context manifests."""
from __future__ import annotations

from collections import defaultdict
import json
import hashlib
import errno
import os
from pathlib import Path, PurePosixPath
import re
import stat
import unicodedata

MAX_SOURCE_BYTES = 2 * 1024 * 1024
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
MAX_ROWS = 4096
MAX_NUMBER = (1 << 53) - 1
HEX = re.compile(r"[0-9a-f]{64}\Z")
# context._slice appends "\n[TRUNCATED: N bytes omitted]\n": 29 bytes plus the
# digits of N, where N = bytes_before - bytes_selected; retained bytes share the payload with it.
TRUNCATION_MARKER_BASE = 29
PHASES = {"orientation", "implementation", "verification", "review", "research"}


class InspectionError(ValueError):
    """Invalid or unsupported metadata; messages never include input content."""


def _integer(value, name):
    if type(value) is not int or not 0 <= value <= MAX_NUMBER:
        raise InspectionError("invalid context manifest: " + name)
    return value


def _text(value, name, limit=4096):
    if not isinstance(value, str) or len(value) > limit:
        raise InspectionError("invalid context manifest: " + name)
    return "".join(" " if ch in "\t\r\n" else "?" if unicodedata.category(ch)[0] == "C"
                   else ch for ch in value)


def _path(value):
    text = _text(value, "source path")
    if (not text or text != value or "\\" in text or ":" in text or text.startswith("~")
            or PurePosixPath(text).is_absolute() or any(p in ("", ".", "..") for p in text.split("/"))):
        raise InspectionError("invalid context manifest: source path must be a relative label")
    return text


def _rows(value, name):
    if not isinstance(value, list) or len(value) > MAX_ROWS:
        raise InspectionError("invalid context manifest: " + name)
    return value


def _paths(value, name):
    paths = [_path(item) for item in _rows(value, name)]
    if len(paths) != len(set(paths)):
        raise InspectionError("invalid context manifest: duplicate " + name)
    return paths


def _digest(value, name, optional=False):
    if optional and value is None:
        return None
    if not isinstance(value, str) or not HEX.fullmatch(value):
        raise InspectionError("invalid context manifest: " + name)
    return value


def _validated(data):
    if not isinstance(data, dict) or type(data.get("schema")) is not int or data["schema"] != 1:
        raise InspectionError("unsupported context manifest schema")
    phase = data.get("phase")
    if not isinstance(phase, str) or phase not in PHASES:
        raise InspectionError("invalid context manifest: phase")
    budget = _integer(data.get("budget_bytes"), "budget_bytes")
    used = _integer(data.get("selected_bytes"), "selected_bytes")
    remaining = _integer(data.get("remaining_bytes"), "remaining_bytes")
    if not budget or used > budget or used + remaining != budget:
        raise InspectionError("invalid context manifest: inconsistent budget")
    targets = _paths(data.get("targets"), "targets")
    missing = _paths(data.get("missing_required"), "missing_required")
    truncated = _paths(data.get("truncated_required"), "truncated_required")
    debt = _integer(data.get("context_debt"), "context_debt")
    if (debt != len(missing) or type(data.get("sufficient")) is not bool
            or data["sufficient"] != (debt == 0) or not set(truncated) <= set(missing)):
        raise InspectionError("invalid context manifest: inconsistent context debt")
    selected = []
    for row in _rows(data.get("selected"), "selected"):
        if not isinstance(row, dict):
            raise InspectionError("invalid context manifest: selected row")
        before = _integer(row.get("bytes_before"), "bytes_before")
        payload = _integer(row.get("bytes_selected"), "bytes_selected")
        omitted = _integer(row.get("omitted_bytes"), "omitted_bytes")
        selection = row.get("selection")
        if (not isinstance(selection, str) or selection not in {"full", "head", "middle", "tail"}
                or omitted > before or payload > before
                or selection == "full" and (omitted != 0 or payload != before)
                or selection != "full" and omitted == 0
                or omitted and before - omitted > 0 and before - omitted + TRUNCATION_MARKER_BASE
                + len(str(before - payload)) > payload):
            raise InspectionError("invalid context manifest: inconsistent source selection")
        selected.append({"path": _path(row.get("path")),
                         "reason": _text(row.get("reason"), "reason"),
                         "bytes_before": before, "bytes_selected": payload,
                         "omitted_bytes": omitted, "selection": selection,
                         "sha256": _digest(row.get("sha256"), "source digest")})
    index = {row["path"]: row for row in selected}
    if len(index) != len(selected) or sum(row["bytes_selected"] for row in selected) > used:
        raise InspectionError("invalid context manifest: duplicate source or payload exceeds bundle")
    partial = {path for path, row in index.items() if row["omitted_bytes"]}
    if any(path not in partial for path in truncated) or (partial & set(missing)) - set(truncated):
        raise InspectionError("invalid context manifest: truncated required source")
    full_paths = {path for path, row in index.items() if row["omitted_bytes"] == 0}
    if not set(targets) - full_paths <= set(missing):
        raise InspectionError("invalid context manifest: target debt is missing")
    if any(path in index and index[path]["omitted_bytes"] == 0 for path in missing):
        raise InspectionError("invalid context manifest: missing source was fully selected")
    omitted_rows = []
    for row in _rows(data.get("omitted"), "omitted"):
        if not isinstance(row, dict):
            raise InspectionError("invalid context manifest: omitted row")
        omitted_rows.append({"path": _path(row.get("path")),
                             "reason": _text(row.get("reason"), "omission reason"),
                             "bytes": _integer(row["bytes"], "omitted bytes") if "bytes" in row else None})
    omitted_paths = [row["path"] for row in omitted_rows]
    if len(omitted_paths) != len(set(omitted_paths)) or set(omitted_paths) & set(index):
        raise InspectionError("invalid context manifest: duplicate omitted source")
    sizes = {}
    for row in selected:
        if row["sha256"] in sizes and sizes[row["sha256"]] != row["bytes_before"]:
            raise InspectionError("invalid context manifest: digest has conflicting source sizes")
        sizes[row["sha256"]] = row["bytes_before"]
    return {"phase": phase, "generated_at": _text(data.get("generated_at"), "generated_at", 80),
            "budget_bytes": budget, "selected_bytes": used, "remaining_bytes": remaining,
            "targets": targets, "selected": selected, "omitted": omitted_rows,
            "missing_required": missing, "truncated_required": truncated,
            "context_debt": debt, "sufficient": data["sufficient"],
            "bundle_sha256": _digest(data.get("bundle_sha256"), "bundle digest"),
            "contract_state_digest": _digest(data.get("contract_state_digest"), "contract digest", optional=True),
            "phase_rules_sha256": (_digest(data["phase_rules_sha256"], "phase rules digest")
                                   if "phase_rules_sha256" in data else None)}


def _bounded(items, limit):
    return {"total": len(items), "items": items[:limit], "omitted": max(0, len(items) - limit)}


def inspect_manifest(data, *, limit=10):
    """Explain recorded byte allocation, omissions and repeated full-source digests."""
    if type(limit) is not int or not 1 <= limit <= 100:
        raise InspectionError("limit must be an integer from 1 to 100")
    value = _validated(data)
    rows = sorted(value["selected"], key=lambda row: (-row["bytes_selected"], row["path"]))
    groups = defaultdict(list)
    for row in rows:
        if row["bytes_before"]:
            groups[row["sha256"]].append(row["path"])
    repeated = [{"sha256": key, "paths": _bounded(sorted(paths), limit)}
                for key, paths in sorted(groups.items()) if len(paths) > 1]
    result = {"schema": 1, "kind": "oms-context-inspection", "validation": "recorded-metadata-only",
            "sources_current": None, "bundle_verified": False,
            "note": "Not the provider's full context; no source or bundle files were read. Repeated source hashes do not prove duplicate selected bytes. No token or billing savings are inferred.",
            "phase": value["phase"], "generated_at": value["generated_at"],
            "budget": {"bytes": value["budget_bytes"], "used_bytes": value["selected_bytes"],
                       "remaining_bytes": value["remaining_bytes"],
                       "source_payload_bytes": sum(row["bytes_selected"] for row in rows),
                       "framing_bytes": value["selected_bytes"] - sum(row["bytes_selected"] for row in rows)},
            "debt": {"reported": value["context_debt"], "reported_sufficient": value["sufficient"],
                     "missing_required": _bounded(value["missing_required"], limit),
                     "truncated_required": _bounded(value["truncated_required"], limit)},
            "targets": _bounded(value["targets"], limit), "sources": _bounded(rows, limit),
            "omissions": _bounded(value["omitted"], limit), "repeated_source_digests": _bounded(repeated, limit),
            "bundle_sha256": value["bundle_sha256"], "contract_state_digest": value["contract_state_digest"]}
    if value["phase_rules_sha256"] is not None:
        result["phase_rules_sha256"] = value["phase_rules_sha256"]
    return result


def compare_manifests(left, right, *, limit=10):
    """Compare recorded packs, without ranking quality or assuming identical tasks."""
    before, after = _validated(left), _validated(right)
    # Validate the display bound even when the result is empty.
    inspect_manifest(left, limit=limit)
    reasons = []
    if before["phase"] != after["phase"]:
        reasons.append("different-phase")
    if set(before["targets"]) != set(after["targets"]):
        reasons.append("different-targets")
    if not before["contract_state_digest"] or not after["contract_state_digest"]:
        reasons.append("contract-identity-unavailable")
    elif before["contract_state_digest"] != after["contract_state_digest"]:
        reasons.append("different-contract-state")
    # A legacy pack has no evidence of the selection policy used by a newer
    # pack. Equal selected files do not repair that missing provenance.
    rules = None
    left_rules, right_rules = before["phase_rules_sha256"], after["phase_rules_sha256"]
    if left_rules is not None or right_rules is not None:
        if left_rules is None or right_rules is None:
            status = "unknown"
            reasons.append("phase-rules-identity-unavailable")
        elif left_rules != right_rules:
            status = "different-recorded-digest"
            reasons.append("different-phase-rules")
        else:
            status = "same-recorded-digest"
        rules = {"left": left_rules, "right": right_rules, "status": status}
    old = {row["path"]: row for row in before["selected"]}
    new = {row["path"]: row for row in after["selected"]}
    changed = []
    for path in sorted(old.keys() & new.keys()):
        fields = [key for key in ("sha256", "bytes_selected", "selection", "omitted_bytes", "reason")
                  if old[path][key] != new[path][key]]
        if fields:
            changed.append({"path": path, "changed_fields": fields})
    result = {"schema": 1, "kind": "oms-context-comparison", "validation": "recorded-metadata-only",
            "recorded_scope_matches": not reasons, "scope_mismatch_reasons": reasons,
            "note": "Deltas describe saved metadata, not quality, current validity, cost savings or a winning run.",
            "delta": {key: after[key] - before[key] for key in
                      ("budget_bytes", "selected_bytes", "remaining_bytes", "context_debt")},
            "added": _bounded(sorted(new.keys() - old.keys()), limit),
            "removed": _bounded(sorted(old.keys() - new.keys()), limit),
            "changed": _bounded(changed, limit)}
    if rules is not None:
        result["phase_rules"] = rules
    return result


def _unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise InspectionError("invalid context manifest: duplicate JSON key")
        result[key] = value
    return result


def read_manifest(repo, raw_path):
    """Descriptor-relative, no-link read of one explicit repo-local regular file."""
    try:
        root = Path(repo).resolve(strict=True)
        path = Path(raw_path)
    except (OSError, ValueError, TypeError):
        raise InspectionError("invalid repository or context manifest path") from None
    if path.is_absolute():
        try:
            path = path.relative_to(root)
        except ValueError:
            raise InspectionError("context manifest must be inside the selected repository") from None
    # Reject traversal even when the requested file would resolve back under the repo.
    if ".." in path.parts or not path.parts or len(path.parts) > 64:
        raise InspectionError("invalid context manifest path")
    if not hasattr(os, "O_NOFOLLOW") or os.open not in os.supports_dir_fd:
        raise InspectionError("safe descriptor-relative reads are unavailable on this host")
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    descriptors = []
    try:
        descriptors.append(os.open(root, directory_flags))
        for part in path.parts[:-1]:
            descriptors.append(os.open(part, directory_flags, dir_fd=descriptors[-1]))
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=descriptors[-1])
        descriptors.append(fd)
        before = os.fstat(fd)
        if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
                or before.st_uid != os.getuid() or before.st_size > MAX_MANIFEST_BYTES):
            raise InspectionError("context manifest must be an owned, bounded regular file without links")
        chunks, remaining = [], MAX_MANIFEST_BYTES + 1
        while remaining:
            chunk = os.read(fd, min(remaining, 65536))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        after = os.fstat(fd)
        fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns", "st_nlink", "st_uid")
        if any(getattr(before, key) != getattr(after, key) for key in fields):
            raise InspectionError("context manifest changed while being read")
        raw = b"".join(chunks)
        if len(raw) > MAX_MANIFEST_BYTES:
            raise InspectionError("context manifest exceeds the read limit")
        def reject_constant(_):
            raise InspectionError("invalid context manifest: non-finite JSON value")
        return json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=reject_constant)
    except (OSError, UnicodeError, RecursionError, ValueError) as exc:
        if isinstance(exc, InspectionError):
            raise
        raise InspectionError("cannot read a valid repo-local context manifest") from None
    finally:
        for fd in reversed(descriptors):
            os.close(fd)


def _source_status(repo, path, expected):
    """Hash a bounded regular file without following any path symlinks."""
    descriptors = []
    try:
        if not hasattr(os, "O_NOFOLLOW") or os.open not in os.supports_dir_fd:
            return "unreadable"
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
        descriptors.append(os.open(Path(repo).resolve(strict=True), flags))
        parts = path.split("/")
        for part in parts[:-1]:
            descriptors.append(os.open(part, flags, dir_fd=descriptors[-1]))
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                     dir_fd=descriptors[-1])
        descriptors.append(fd)
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_SOURCE_BYTES:
            return "unreadable"
        digest = hashlib.sha256()
        remaining = MAX_SOURCE_BYTES + 1
        while remaining:
            chunk = os.read(fd, min(remaining, 65536))
            if not chunk:
                break
            digest.update(chunk)
            remaining -= len(chunk)
        after = os.fstat(fd)
        fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
        if not remaining or any(getattr(before, k) != getattr(after, k) for k in fields):
            return "unreadable"
        return "unchanged" if digest.hexdigest() == expected else "changed"
    except OSError as exc:
        return "missing" if exc.errno == errno.ENOENT else "unreadable"
    finally:
        for fd in reversed(descriptors):
            os.close(fd)


def inspect_files(repo, manifest, *, compare=None, limit=10, verify_current=False):
    if compare is not None and verify_current:
        raise InspectionError("--verify-current cannot be combined with --compare")
    left = read_manifest(repo, manifest)
    if compare is not None:
        return compare_manifests(left, read_manifest(repo, compare), limit=limit)
    result = inspect_manifest(left, limit=limit)
    if verify_current:
        counts = {key: 0 for key in ("unchanged", "changed", "missing", "unreadable")}
        paths = {key: [] for key in ("changed", "missing")}
        for row in sorted(_validated(left)["selected"], key=lambda row: row["path"]):
            status = _source_status(repo, row["path"], row["sha256"])
            counts[status] += 1
            if status in paths:
                paths[status].append(row["path"])
        counts.update({key + "_paths": _bounded(value, limit) for key, value in paths.items()})
        result["sources_current"] = counts
        result["note"] = ("Not the provider's full context; the bundle was not read. "
                          "Repeated source hashes do not prove duplicate selected bytes. "
                          "No token or billing savings are inferred.")
    return result
