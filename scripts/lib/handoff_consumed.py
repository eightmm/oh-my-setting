#!/usr/bin/env python3
"""Advisory pickup marker for handoff digests.

The resume hook records which session was shown which digest as one append-only
row in `.oms/handoffs/consumed.jsonl`, written through durable-jsonl.py. The
file is derived, advisory state: it never hides a digest, never counts as
approval or verification evidence, and a failed write is silent because resume
must not fail on it. Readers see the first pickup and how many distinct
sessions were shown the digest.
"""

import argparse
import importlib.util
import json
import os
import re
import sys
import time

FILE = "consumed.jsonl"
LABEL = "handoff-consumed.jsonl"
TAIL_BYTES = 1024 * 1024
NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,200}\.md$")


def clean(value, limit):
    text = " ".join("".join(ch if ch.isprintable() else " " for ch in str(value)).split())
    return text[:limit]


def payload_session(text):
    try:
        value = json.loads(text).get("session_id", "")
    except (ValueError, AttributeError):
        return ""
    return clean(value, 120) if isinstance(value, str) else ""


def _directory(repo):
    root = os.path.join(repo, ".oms", "handoffs")
    parts = (os.path.join(repo, ".oms"), root)
    if any(os.path.islink(part) for part in parts):
        return None
    return root


def pickups(repo):
    """name -> {"session", "ts", "count"} for the first pickup; {} when unreadable."""
    root = _directory(repo)
    found = {}
    if root is None:
        return found
    path = os.path.join(root, FILE)
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except OSError:
        return found
    try:
        with os.fdopen(descriptor, "rb") as handle:
            size = os.fstat(handle.fileno()).st_size
            skip_first = size > TAIL_BYTES
            if skip_first:
                handle.seek(size - TAIL_BYTES)
            data = handle.read(TAIL_BYTES)
    except OSError:
        return found
    lines = data.decode("utf-8", errors="replace").splitlines()
    if skip_first and lines:
        lines = lines[1:]
    sessions = {}
    for line in lines:
        try:
            row = json.loads(line)
            name, session, ts = row["handoff"], row["session"], row["ts"]
        except (ValueError, KeyError, TypeError):
            continue
        if not all(isinstance(item, str) for item in (name, session, ts)):
            continue
        seen = sessions.setdefault(name, set())
        if session in seen:
            continue
        seen.add(session)
        if name not in found:
            found[name] = {"session": clean(session, 120), "ts": clean(ts, 40), "count": 0}
        found[name]["count"] += 1
    return found


def record(repo, name, session):
    """Append one pickup row; True when written. Never raises."""
    try:
        root = _directory(repo)
        if root is None or not NAME.match(name):
            return False
        digest = os.path.join(root, name)
        if os.path.islink(digest) or not os.path.isfile(digest):
            return False
        session = clean(session, 120) or "unknown"
        prior = pickups(repo).get(name)
        if prior and prior["session"] == session:
            return False
        spec = importlib.util.spec_from_file_location(
            "oms_durable_jsonl", os.path.join(os.path.dirname(os.path.abspath(__file__)), "durable-jsonl.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        row = {"schema": 1, "kind": "handoff-consumed", "handoff": name, "session": session,
               "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
        module.append(os.path.join(root, FILE),
                      (json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8"), LABEL)
        return True
    except BaseException:  # durable-jsonl reports through SystemExit
        return False


def main():
    parser = argparse.ArgumentParser(description="Record a handoff pickup (best effort).")
    parser.add_argument("--repo", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--session", default="")
    args = parser.parse_args()
    session = args.session or payload_session(sys.stdin.read(1 << 20) if not sys.stdin.isatty() else "")
    record(args.repo, args.name, session)
    return 0


if __name__ == "__main__":
    sys.exit(main())
