#!/usr/bin/env python3
"""One repo-bound copy-on-write store for every artifact-index mutation."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import runpy
import stat
import sys
import time
import uuid
from typing import Callable, Iterable, Optional, Sequence, Tuple


LIB_DIR = os.path.dirname(os.path.abspath(__file__))
DURABLE = runpy.run_path(os.path.join(LIB_DIR, "durable-jsonl.py"))
RETENTION = runpy.run_path(os.path.join(LIB_DIR, "artifact-index-retention.py"))
MAX_INDEX_BYTES = DURABLE["MAX_FILE_BYTES"]
MAX_INPUT_BYTES = DURABLE["MAX_RECOVERY_BYTES"]
MAX_ROW_BYTES = DURABLE["MAX_ROW_BYTES"]


def fail(message: str) -> None:
    print("error: artifact index: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def retention_limits(
    keep: Optional[int] = None, high: Optional[int] = None
) -> Tuple[int, int]:
    if keep is None:
        try:
            keep = int(os.environ.get("OMS_ARTIFACT_INDEX_KEEP", "1000"))
        except ValueError:
            keep = 1000
    if high is None:
        try:
            high = int(os.environ.get("OMS_ARTIFACT_INDEX_HIGH_WATER", "1200"))
        except ValueError:
            high = 1200
    if keep <= 0 or high < keep:
        fail("retention requires positive keep and high-water >= keep")
    return keep, high


def canonical_index(
    repo: str,
    index: str,
    create_parent: bool = False,
    ensure_ignore: bool = False,
) -> str:
    root = DURABLE["physical_root"](repo, "artifact index")
    target = DURABLE["canonical_repo_path"](
        repo, index, "artifact index", create_parent
    )
    if ensure_ignore:
        ensure_oms_ignore(root)
    return target


def ensure_oms_ignore(repo: str) -> None:
    root = DURABLE["physical_root"](repo, "artifact index")
    ignore = os.path.join(root, ".oms", ".gitignore")

    def initialize(old: bytes) -> bytes:
        return b"*\n"

    DURABLE["atomic_mutate"](
        ignore,
        initialize,
        root,
        missing_ok=True,
        create_parent=True,
        max_output=MAX_ROW_BYTES,
        label=".oms ignore",
        preserve_existing=True,
    )


def _reject_json_constant(value: str) -> None:
    raise ValueError("non-finite JSON value: %s" % value)


def parse_index_line(line: bytes, lineno: int) -> dict:
    """Parse one LF-delimited finite JSON object, preserving CRLF on disk."""
    if not isinstance(line, bytes) or not line.endswith(b"\n"):
        raise ValueError("line %d does not end in LF" % lineno)
    if len(line) > MAX_ROW_BYTES:
        raise ValueError(
            "line %d exceeds the %d-byte row limit" % (lineno, MAX_ROW_BYTES)
        )
    payload = line[:-1]
    if payload.endswith(b"\r"):
        payload = payload[:-1]
    if b"\r" in payload:
        raise ValueError("line %d contains a bare CR" % lineno)
    if not payload.strip():
        raise ValueError("line %d is blank" % lineno)
    if b"\x00" in payload:
        raise ValueError("line %d contains a NUL byte" % lineno)
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ValueError("invalid UTF-8 at line %d: %s" % (lineno, exc))
    try:
        row = json.loads(text, parse_constant=_reject_json_constant)
    except ValueError as exc:
        raise ValueError("invalid JSON at line %d: %s" % (lineno, exc))
    if not isinstance(row, dict):
        raise ValueError("line %d is not an object" % lineno)
    return row


def _validated_lines(body: bytes) -> list[bytes]:
    if not body:
        return []
    if not body.endswith(b"\n"):
        fail("has a partial final JSONL row")
    # JSONL is LF-delimited. bytes.splitlines() also treats bare CR as a row
    # boundary, which can make one invalid physical row look like two valid
    # objects. Preserve each exact LF row; parse_index_line alone handles the
    # optional CR immediately before LF.
    lines = [payload + b"\n" for payload in body[:-1].split(b"\n")]
    for lineno, line in enumerate(lines, 1):
        try:
            parse_index_line(line, lineno)
        except ValueError as exc:
            fail(str(exc))
    return lines


def read_index(repo: str, index: str, missing_ok: bool = False) -> bytes:
    target = canonical_index(repo, index, False, False)
    body = DURABLE["read_no_follow"](
        repo,
        target,
        "artifact index",
        missing_ok=missing_ok,
        max_bytes=MAX_INPUT_BYTES,
    )
    _validated_lines(body)
    return body


def read_raw_index(repo: str, index: str) -> bytes:
    """Read one stable, bounded index snapshot without accepting its shape."""
    target = canonical_index(repo, index, False, False)
    return DURABLE["read_no_follow"](
        repo,
        target,
        "artifact index salvage source",
        missing_ok=False,
        max_bytes=MAX_INPUT_BYTES,
    )


def _salvage_rows(raw: bytes) -> Tuple[list[bytes], int, int]:
    """Return structurally complete JSON-object rows without exposing rejects."""
    recovered = []
    total = 0
    dropped = 0
    parts = raw.split(b"\n")
    final = len(parts) - 1
    for index, payload in enumerate(parts):
        # split() creates one empty terminator after a final LF. It is not a
        # blank row; every earlier empty payload is an actual blank JSONL row.
        if index == final and payload == b"" and raw.endswith(b"\n"):
            continue
        if index == final and payload == b"" and not raw:
            continue
        total += 1
        candidate = payload + b"\n"
        try:
            parse_index_line(candidate, total)
        except ValueError:
            dropped += 1
            continue
        recovered.append(candidate)
    return recovered, total, dropped


def _salvage_receipt(
    digest: str,
    quarantine_relative: str,
    raw_bytes: int,
    source_rows: int,
    recovered_rows: int,
    dropped_rows: int,
    compacted_rows: int,
    event_id: str,
) -> dict:
    now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return {
        "schema": 1,
        "event_id": event_id,
        "operation_id": "op_salvage_" + digest[:32],
        "artifact_id": "sha256:" + digest,
        "ts": now,
        "kind": "artifact-index-salvage",
        "provider": "local",
        "exit": 0,
        "artifact": quarantine_relative,
        "quarantine": quarantine_relative,
        "quarantine_sha256": digest,
        "raw_bytes": raw_bytes,
        "source_rows": source_rows,
        "recovered_rows": recovered_rows,
        "dropped_rows": dropped_rows,
        "compacted_rows": compacted_rows,
    }


def _salvage_body(
    recovered: Sequence[bytes],
    receipt_base: dict,
    keep: int,
    high: int,
) -> Tuple[bytes, dict]:
    """Reach a stable compacted-row count while requiring the receipt."""
    compacted = 0
    body = b""
    receipt = dict(receipt_base)
    for _attempt in range(16):
        receipt["compacted_rows"] = compacted
        receipt_line, _event_id = _row_bytes(receipt)
        candidate = list(recovered) + [receipt_line]
        candidate_body = b"".join(candidate)
        if len(candidate) > high or len(candidate_body) > MAX_INDEX_BYTES:
            body = _bounded(
                candidate,
                keep,
                MAX_INDEX_BYTES,
                [receipt["event_id"]],
            )
        else:
            body = candidate_body
        # The required receipt is the final source row and retention preserves
        # source order. Counting retained rows avoids assuming legacy objects
        # have unique (or any) event_id merely to report compaction honestly.
        next_compacted = len(recovered) - (len(body.splitlines()) - 1)
        if next_compacted == compacted:
            _validated_lines(body)
            return body, receipt
        compacted = next_compacted
    fail("salvage retention counts did not stabilize")


def _verify_quarantine(repo: str, target: str, raw: bytes) -> None:
    verified = DURABLE["read_no_follow"](
        repo,
        target,
        "artifact index quarantine",
        missing_ok=False,
        max_bytes=MAX_INPUT_BYTES,
    )
    if verified != raw:
        fail("quarantine verification mismatch")
    try:
        info = os.lstat(target)
    except OSError as exc:
        fail("cannot verify quarantine mode: %s" % exc)
    if not DURABLE["safe_regular"](info):
        fail("quarantine must be a regular non-symlink, non-hard-linked file")
    if os.name != "nt" and stat.S_IMODE(info.st_mode) != 0o600:
        fail("quarantine must have mode 0600")


def _ensure_quarantine(repo: str, relative: str, raw: bytes) -> str:
    target = DURABLE["canonical_repo_path"](
        repo, relative, "artifact index quarantine", True
    )

    try:
        existing = os.lstat(target)
    except FileNotFoundError:
        existing = None
    except OSError as exc:
        fail("cannot inspect quarantine collision: %s" % exc)
    if existing is not None:
        if not DURABLE["safe_regular"](existing):
            fail("quarantine must be a regular non-symlink, non-hard-linked file")
        try:
            _verify_quarantine(repo, target, raw)
        except SystemExit:
            fail("content-addressed quarantine collision")
        return target

    def create_only(old: bytes) -> bytes:
        return raw

    stored = DURABLE["atomic_mutate"](
        target,
        create_only,
        repo,
        missing_ok=True,
        create_parent=True,
        max_output=MAX_INPUT_BYTES,
        max_input=MAX_INPUT_BYTES,
        label="artifact index quarantine",
        expected_exists=False,
    )
    if stored != raw:
        fail("quarantine verification mismatch")
    _verify_quarantine(repo, target, raw)
    return target


def _replace_corrupt_index(
    repo: str, index: str, expected: bytes, repaired: bytes
) -> bytes:
    target = canonical_index(repo, index, False, False)
    _validated_lines(repaired)

    def compare_and_replace(old: bytes) -> bytes:
        if old != expected:
            fail("changed after the salvage decision")
        return repaired

    stored = DURABLE["atomic_mutate"](
        target,
        compare_and_replace,
        repo,
        missing_ok=False,
        create_parent=False,
        max_output=MAX_INDEX_BYTES,
        max_input=MAX_INPUT_BYTES,
        label="artifact index salvage",
    )
    _validated_lines(stored)
    return stored


def salvage_index(repo: str, index: str, *, apply: bool = False) -> dict:
    """Plan or repair structural JSONL corruption under the caller's lock."""
    root = DURABLE["physical_root"](repo, "artifact index")
    target = canonical_index(root, index, False, False)
    raw = read_raw_index(root, target)
    recovered, source_rows, dropped = _salvage_rows(raw)
    recovered_body = b"".join(recovered)
    if dropped == 0 and recovered_body == raw:
        return {
            "healthy": True,
            "applied": False,
            "raw_bytes": len(raw),
            "source_rows": source_rows,
            "recovered_rows": len(recovered),
            "dropped_rows": 0,
            "compacted_rows": 0,
        }

    digest = hashlib.sha256(raw).hexdigest()
    quarantine_relative = (
        ".oms/artifacts/quarantine/artifact-index-%s.raw" % digest
    )
    existing_ids = set()
    for line in recovered:
        event_id = json.loads(line.decode("utf-8")).get("event_id")
        if isinstance(event_id, str):
            existing_ids.add(event_id)
    while True:
        receipt_event = "evt_salvage_" + uuid.uuid4().hex
        if receipt_event not in existing_ids:
            break
    keep, high = retention_limits()
    receipt_base = _salvage_receipt(
        digest,
        quarantine_relative,
        len(raw),
        source_rows,
        len(recovered),
        dropped,
        0,
        receipt_event,
    )
    repaired, receipt = _salvage_body(recovered, receipt_base, keep, high)
    result = {
        "healthy": False,
        "applied": apply,
        "digest": digest,
        "quarantine": quarantine_relative,
        "raw_bytes": len(raw),
        "source_rows": source_rows,
        "recovered_rows": len(recovered),
        "dropped_rows": dropped,
        "compacted_rows": receipt["compacted_rows"],
        "stored_rows": len(repaired.splitlines()),
    }
    if not apply:
        return result

    quarantine = _ensure_quarantine(root, quarantine_relative, raw)
    stored = _replace_corrupt_index(root, target, raw, repaired)
    if stored != repaired:
        fail("repaired index verification mismatch")
    if read_index(root, target) != repaired:
        fail("repaired index verification mismatch")
    _verify_quarantine(root, quarantine, raw)
    return result


def _row_bytes(row: object) -> Tuple[bytes, str]:
    if isinstance(row, dict):
        data = (json.dumps(row, ensure_ascii=False, allow_nan=False) + "\n").encode(
            "utf-8"
        )
        event_id = row.get("event_id")
    elif isinstance(row, bytes):
        data = row
        lines = _validated_lines(data)
        if len(lines) != 1:
            fail("an appended artifact event must be exactly one JSONL row")
        event_id = json.loads(data.decode("utf-8")).get("event_id")
    else:
        fail("an appended artifact event must be an object or bytes")
    if len(data) > MAX_ROW_BYTES:
        fail("an appended row exceeds the %d-byte row limit" % MAX_ROW_BYTES)
    if not isinstance(event_id, str) or not event_id:
        fail("an appended row requires event_id")
    return data, event_id


def _bounded(
    lines: Iterable[bytes],
    keep: int,
    max_bytes: int,
    required_event_ids: Sequence[str] = (),
) -> bytes:
    try:
        retained = RETENTION["retained_lines"](
            lines,
            keep,
            max_bytes=max_bytes,
            required_event_ids=required_event_ids,
        )
    except ValueError as exc:
        fail(str(exc))
    body = b"".join(retained)
    if len(retained) > keep or len(body) > max_bytes:
        fail("retention could not satisfy the durable bounds")
    return body


def mutate_index(
    repo: str,
    index: str,
    transform: Callable[[bytes], bytes],
    *,
    missing_ok: bool = False,
    create_parent: bool = False,
    expected: Optional[bytes] = None,
) -> bytes:
    target = canonical_index(repo, index, create_parent, False)

    def checked(old: bytes) -> bytes:
        _validated_lines(old)
        if expected is not None and old != expected:
            fail("changed after the mutation decision")
        new = transform(old)
        _validated_lines(new)
        return new

    return DURABLE["atomic_mutate"](
        target,
        checked,
        repo,
        missing_ok=missing_ok,
        create_parent=create_parent,
        max_output=MAX_INDEX_BYTES,
        max_input=MAX_INPUT_BYTES,
        label="artifact index",
    )


def append_rows(
    repo: str,
    index: str,
    rows: Iterable[object],
    *,
    keep: Optional[int] = None,
    high: Optional[int] = None,
    expected: Optional[bytes] = None,
) -> bytes:
    keep, high = retention_limits(keep, high)
    encoded = []
    required = []
    for row in rows:
        data, event_id = _row_bytes(row)
        encoded.append(data)
        required.append(event_id)
    if not encoded:
        return read_index(repo, index, missing_ok=True)
    if len(set(required)) != len(required):
        fail("one append cannot contain duplicate event ids")

    def append_to(old: bytes) -> bytes:
        old_lines = _validated_lines(old)
        old_ids = set()
        for line in old_lines:
            event_id = json.loads(line.decode("utf-8")).get("event_id")
            if isinstance(event_id, str):
                old_ids.add(event_id)
        duplicate = old_ids.intersection(required)
        if duplicate:
            fail("duplicate artifact event id: %s" % sorted(duplicate)[0])
        candidate = old_lines + encoded
        candidate_body = b"".join(candidate)
        if len(candidate) > high or len(candidate_body) > MAX_INDEX_BYTES:
            return _bounded(candidate, keep, MAX_INDEX_BYTES, required)
        return candidate_body

    return mutate_index(
        repo,
        index,
        append_to,
        missing_ok=True,
        create_parent=True,
        expected=expected,
    )


def replace_bytes(
    repo: str,
    index: str,
    body: bytes,
    *,
    keep: Optional[int] = None,
    expected: Optional[bytes] = None,
    required_event_ids: Sequence[str] = (),
) -> bytes:
    keep, _high = retention_limits(keep, keep)
    lines = _validated_lines(body)
    bounded = _bounded(lines, keep, MAX_INDEX_BYTES, required_event_ids)
    return mutate_index(
        repo,
        index,
        lambda _old: bounded,
        missing_ok=False,
        create_parent=False,
        expected=expected,
    )


def delete_orphans(
    repo: str,
    index: str,
    kept_body: bytes,
    *,
    dry_run: bool,
    grace: int,
) -> Tuple[list[str], int]:
    """Delete only owned, single-link regular files below the physical root."""
    if grace < 0:
        fail("orphan grace must be non-negative")
    root = DURABLE["physical_root"](repo, "artifact index")
    target = canonical_index(root, index, False, False)
    artifacts_root = os.path.join(root, ".oms", "artifacts")
    # Re-validates every intermediate component without resolving a junction.
    DURABLE["canonical_repo_path"](
        root, os.path.join(artifacts_root, ".boundary"), "artifact root", False
    )
    try:
        artifacts_info = os.lstat(artifacts_root)
    except FileNotFoundError:
        # A custom repo-bound ledger need not own the default artifact tree.
        # Missing means there is simply no cleanup namespace yet; a planted
        # component was already rejected by canonical_repo_path above.
        return [], 0
    except OSError as exc:
        fail("cannot inspect artifact root: %s" % exc)
    if (stat.S_ISLNK(artifacts_info.st_mode) or
            DURABLE["is_reparse"](artifacts_info) or
            not stat.S_ISDIR(artifacts_info.st_mode)):
        fail("artifact root must be a real directory")
    root_snapshot = DURABLE["component_snapshot"](
        root, os.path.join(artifacts_root, ".boundary"), "artifact root"
    )

    referenced = set()
    for line in _validated_lines(kept_body):
        row = json.loads(line.decode("utf-8"))
        for key in ("artifact", "patch", "source"):
            value = row.get(key)
            if not isinstance(value, str) or not value:
                continue
            portable = value.replace("\\", "/")
            drive_path = (
                len(portable) >= 3 and portable[0].isalpha() and
                portable[1] == ":" and portable[2] == "/"
            )
            native_absolute = os.path.isabs(value)
            if ((portable.startswith("/") or drive_path) and
                    not native_absolute):
                # A foreign-platform absolute spelling cannot be resolved
                # safely on this host. Validation owns the malformed row;
                # cleanup must not treat the spelling as a delete target.
                continue
            candidate = os.path.realpath(
                value if native_absolute else
                os.path.join(root, *portable.split("/")))
            if not DURABLE["_inside"](candidate, root):
                continue
            if not DURABLE["_inside"](candidate, artifacts_root):
                # Repo-bound custom artifact/source paths are valid evidence,
                # but this cleanup owns only .oms/artifacts. They can neither
                # protect nor nominate a file inside the owned delete root.
                continue
            referenced.add(candidate)

    now = time.time()
    candidates = []
    fresh = 0
    DURABLE["components_unchanged"](root_snapshot, "artifact root")
    for dirpath, dirnames, filenames in os.walk(artifacts_root, followlinks=False):
        safe_dirs = []
        for name in dirnames:
            if name.endswith(".lock"):
                continue
            path = os.path.join(dirpath, name)
            try:
                info = os.lstat(path)
            except OSError:
                continue
            if (stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode) and
                    not DURABLE["is_reparse"](info)):
                safe_dirs.append(name)
        dirnames[:] = safe_dirs
        for name in filenames:
            path = os.path.join(dirpath, name)
            if name in ("index.jsonl", ".gitignore") or name.endswith(".lock"):
                continue
            try:
                info = os.lstat(path)
            except OSError:
                continue
            if not DURABLE["safe_regular"](info):
                # Cleanup owns only single-link regular artifact bodies.
                # Symlinks, reparse points, hard links, devices, and sockets
                # are preserved without letting one planted entry abort safe
                # cleanup elsewhere in the tree.
                continue
            canonical = DURABLE["canonical_repo_path"](
                root, path, "orphan candidate", False
            )
            if canonical == target or canonical in referenced:
                continue
            if now - info.st_mtime < grace:
                fresh += 1
                continue
            candidates.append((canonical, info))

    DURABLE["components_unchanged"](root_snapshot, "artifact root")
    changed = []
    for path, before in sorted(candidates):
        relative = os.path.relpath(path, root)
        if not dry_run:
            DURABLE["components_unchanged"](root_snapshot, "artifact root")
            try:
                after = os.lstat(path)
            except FileNotFoundError:
                continue
            except OSError as exc:
                fail("cannot recheck orphan %s: %s" % (relative, exc))
            if (not DURABLE["safe_regular"](after) or
                    not DURABLE["same_file"](before, after)):
                fail("orphan changed before delete: %s" % relative)
            if os.name == "nt":
                os.unlink(path)
            else:
                parent = os.path.dirname(path)
                parent_before = os.lstat(parent)
                descriptor = os.open(
                    parent, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
                )
                try:
                    if not DURABLE["same_file"](parent_before, os.fstat(descriptor)):
                        fail("orphan parent changed before delete: %s" % relative)
                    anchored = os.stat(
                        os.path.basename(path), dir_fd=descriptor,
                        follow_symlinks=False)
                    if (not DURABLE["safe_regular"](anchored) or
                            not DURABLE["same_file"](before, anchored)):
                        fail("orphan changed before anchored delete: %s" % relative)
                    os.unlink(os.path.basename(path), dir_fd=descriptor)
                finally:
                    os.close(descriptor)
        changed.append(relative)
    return changed, fresh


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="action", required=True)
    canonical = subparsers.add_parser("canonical")
    canonical.add_argument("--repo", required=True)
    canonical.add_argument("--index", required=True)
    canonical.add_argument("--create-parent", action="store_true")
    canonical.add_argument("--ensure-ignore", action="store_true")
    args = parser.parse_args()
    if args.action == "canonical":
        print(
            canonical_index(
                args.repo,
                args.index,
                create_parent=args.create_parent,
                ensure_ignore=args.ensure_ignore,
            )
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
