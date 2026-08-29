#!/usr/bin/env python3
"""Bounded, no-follow PROJECT.md publication-state snapshot."""

from __future__ import print_function

import hashlib
import json
import os
import stat
import sys


MAX_PROJECT_BYTES = 1024 * 1024


def _is_reparse(info):
    attributes = getattr(info, "st_file_attributes", 0)
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & marker)


def _unhealthy(present, error):
    return {
        "schema": 1,
        "present": bool(present),
        "healthy": False,
        "state": "invalid",
        "sha256": "",
        "error": error,
    }


def snapshot(path):
    """Return one exact raw-byte snapshot without following the leaf."""
    try:
        before = os.lstat(path)
    except FileNotFoundError:
        return {
            "schema": 1,
            "present": False,
            "healthy": True,
            "state": "missing",
            "sha256": "",
            "error": "",
        }
    except OSError:
        return _unhealthy(True, "project-unreadable")
    if (not stat.S_ISREG(before.st_mode) or _is_reparse(before) or
            before.st_nlink != 1 or before.st_size > MAX_PROJECT_BYTES):
        return _unhealthy(True, "project-unsafe-file")

    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError:
        return _unhealthy(True, "project-unreadable")
    try:
        opened = os.fstat(descriptor)
        if (not stat.S_ISREG(opened.st_mode) or _is_reparse(opened) or
                opened.st_nlink != 1 or
                (opened.st_dev, opened.st_ino) !=
                (before.st_dev, before.st_ino)):
            return _unhealthy(True, "project-changed-while-opening")
        chunks = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(65536, MAX_PROJECT_BYTES + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_PROJECT_BYTES:
                return _unhealthy(True, "project-too-large")
        after = os.fstat(descriptor)
        stable_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns",
                         "st_ctime_ns", "st_nlink")
        if any(getattr(opened, key) != getattr(after, key)
               for key in stable_fields):
            return _unhealthy(True, "project-changed-while-reading")
    except OSError:
        return _unhealthy(True, "project-unreadable")
    finally:
        os.close(descriptor)

    payload = b"".join(chunks)
    try:
        text = payload.decode("utf-8")
    except UnicodeError:
        return _unhealthy(True, "project-not-utf8")
    states = []
    for raw in text.split("\n"):
        line = raw[:-1] if raw.endswith("\r") else raw
        if not line.startswith("- State:"):
            continue
        states.append(line[len("- State:"):].strip())
    if not states:
        state_value = "missing"
    elif len(states) != 1:
        state_value = "invalid"
    elif states[0] == "draft":
        state_value = "draft"
    elif states[0] == "confirmed":
        state_value = "confirmed"
    elif states[0] == "active":
        state_value = "legacy-active"
    else:
        state_value = "invalid"
    return {
        "schema": 1,
        "present": True,
        "healthy": True,
        "state": state_value,
        "sha256": hashlib.sha256(payload).hexdigest(),
        "error": "",
    }


def _usage():
    print("usage: project-state.py state|snapshot FILE", file=sys.stderr)
    return 2


def main(argv):
    if len(argv) != 3 or argv[1] not in ("state", "snapshot"):
        return _usage()
    row = snapshot(argv[2])
    if argv[1] == "state":
        print(row["state"] if row["healthy"] else "invalid")
    else:
        print(json.dumps(row, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
