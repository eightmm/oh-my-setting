#!/usr/bin/env python3
"""Read-only, mechanically linked failure/resolution candidates; never learn or retire."""
import argparse
from collections import deque
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from agent_memory_review import scan  # noqa: E402 (shared, fail-closed sensitive scan)

MAX_BYTES = 16 * 1024 * 1024
MAX_ROW_BYTES = 128 * 1024
MAX_CANDIDATES = 100
TEXT_LIMIT = 1000


def valid(row):
    # Match list/show's legacy and schema-2 replay contract. Unknown fields,
    # including untyped receipt claims, grant no verification status.
    if not isinstance(row, dict):
        return False
    schema = row.get("schema", 1)
    if type(schema) is not int or schema not in (1, 2):
        return False
    if row.get("event") not in ("fail", "resolved"):
        return False
    if not isinstance(row.get("fingerprint"), str) or not row["fingerprint"].strip():
        return False
    for key in ("fingerprint", "ts", "agent", "kind", "cmd", "summary", "next", "how",
                "state_fingerprint", "failure_code", "recovery"):
        if key in row:
            if not isinstance(row[key], str):
                return False
            # JSON escapes can carry lone surrogates. Quarantine that row
            # before one encoding failure withholds the entire scanner batch.
            try:
                row[key].encode("utf-8")
            except UnicodeEncodeError:
                return False
    for key in ("exit", "count"):
        if key in row and (type(row[key]) is not int or row[key] < 0):
            return False
    if "resolved" in row and not isinstance(row["resolved"], bool):
        return False
    if schema == 1:
        return True
    if not row.get("ts", "").strip() or not row.get("agent", "").strip():
        return False
    return row["event"] == "resolved" or (
        bool(row.get("kind", "").strip()) and bool(row.get("cmd", "").strip())
        and type(row.get("exit")) is int and row["exit"] >= 0)


def source(number, raw):
    # Logical source name deliberately avoids disclosing a custom ledger path.
    return {"source": "failure-ledger", "line": number,
            "sha256": hashlib.sha256(raw.rstrip(b"\r\n")).hexdigest(), "status": "recorded"}


def report(path, limit):
    pending, current = {}, {}
    closed = deque(maxlen=MAX_CANDIDATES)
    invalid = total = duplicate_resolutions = unmatched_resolutions = 0
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        return {"schema": 1, "candidates": [], "invalid_rows": 0, "omitted": 0,
                "source_status": "missing"}
    with os.fdopen(fd, "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_BYTES:
            raise ValueError("lessons requires a regular ledger of at most 16 MiB")
        # Cap the snapshot even if an append races the initial size check.
        data = handle.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("lessons ledger exceeds 16 MiB")
    for number, raw in enumerate(data.splitlines(), 1):
        if not raw.strip():
            continue
        try:
            row = json.loads(raw) if len(raw) <= MAX_ROW_BYTES else None
        except (ValueError, UnicodeError, RecursionError):
            row = None
        if not valid(row):
            invalid += 1
            continue
        fp = row["fingerprint"]
        ref = source(number, raw)
        previous = current.get(fp)
        current[fp] = (row["event"], ref)
        if row["event"] == "fail":
            if fp not in pending:
                pending[fp] = {"first": row, "first_source": ref, "count": 0}
            pending[fp]["count"] += 1
            pending[fp]["last_source"] = ref
        elif fp in pending:
            episode = pending.pop(fp)
            episode.update(resolution=row, resolution_source=ref)
            closed.append(episode)
            total += 1
        elif previous and previous[0] == "resolved":
            duplicate_resolutions += 1
        else:
            unmatched_resolutions += 1
    selected = list(reversed(closed))[:limit]
    # Scan original, untruncated free text; truncation must not hide a secret
    # from the established scanner. Nothing outside these typed fields emits.
    texts = []
    for episode in selected:
        first, resolution = episode["first"], episode["resolution"]
        texts.append("\n".join(first.get(k, "") for k in
                               ("fingerprint", "kind", "failure_code", "cmd", "summary"))
                     + "\n" + resolution.get("how", ""))
    try:
        clean = scan(texts)
    except Exception:
        clean = [False] * len(texts)
    candidates = []
    for episode, allowed in zip(selected, clean):
        first, resolution = episode["first"], episode["resolution"]
        fp = first["fingerprint"]
        fields = {key: first.get(key) for key in
                  ("fingerprint", "kind", "failure_code", "cmd", "summary")}
        fields["how"] = resolution.get("how")
        truncated = any(isinstance(v, str) and len(v) > TEXT_LIMIT for v in fields.values())
        fields = {key: (value[:TEXT_LIMIT] if allowed and isinstance(value, str) else None)
                  for key, value in fields.items()}
        candidates.append({
            "status": "candidate", "verification_status": "unverified",
            "verification_evidence": "missing", "content_status": "recorded" if allowed else "withheld",
            "text_truncated": truncated,
            "failure": {key: value for key, value in fields.items() if key != "how"},
            "occurrences": episode["count"],
            "failure_source": episode["first_source"],
            "last_failure_source": episode["last_source"],
            "resolution": {"how": fields["how"], "source": episode["resolution_source"],
                           "note_status": ("withheld" if not allowed else
                                           "recorded" if resolution.get("how", "").strip() else "missing")},
            "last_recorded_status": "open" if current[fp][0] == "fail" else "resolved",
            "last_recorded_source": current[fp][1],
            "status_basis": "last valid ledger event; hook TTL and Git state are not evaluated",
        })
    return {"schema": 1, "candidates": candidates, "source_status": "recorded",
            "invalid_rows": invalid, "omitted": max(0, total - len(candidates)),
            "duplicate_resolutions": duplicate_resolutions,
            "unmatched_resolutions": unmatched_resolutions}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", required=True)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--limit", type=int, default=0)
    args = parser.parse_args()
    if not 0 <= args.limit <= MAX_CANDIDATES:
        parser.error("--limit must be between 0 and 100 (0 uses default 20)")
    try:
        doc = report(args.ledger, args.limit or 20)
    except (OSError, ValueError):
        # Do not print exception text: filesystem errors may disclose paths.
        parser.exit(2, "error: cannot read lessons: regular non-symlink ledger up to 16 MiB required\n")
    if args.json:
        print(json.dumps(doc, ensure_ascii=False, allow_nan=False))
        return
    print("Failure lessons: candidate / unverified; recorded resolutions prove no success")
    for item in doc["candidates"]:
        row = item["failure"]
        print("%s  candidate/unverified kind=%s count=%d last-recorded=%s truncated=%s" % (
            json.dumps(row["fingerprint"] or "(withheld)", ensure_ascii=False),
            json.dumps(row["kind"], ensure_ascii=False), item["occurrences"],
            item["last_recorded_status"], str(item["text_truncated"]).lower()))
        for label, text in (("failure", row["summary"] or row["cmd"]),
                            ("recorded resolution", item["resolution"]["how"])):
            print("  %s: %s" % (label, json.dumps(text, ensure_ascii=False)))
        print("  source: failure-ledger:%d -> failure-ledger:%d; note=%s; verification=missing; content=%s" % (
            item["failure_source"]["line"], item["resolution"]["source"]["line"],
            item["resolution"]["note_status"], item["content_status"]))
    print("candidates=%d omitted=%d invalid_rows=%d source=%s" % (
        len(doc["candidates"]), doc["omitted"], doc["invalid_rows"], doc["source_status"]))


if __name__ == "__main__":
    main()
