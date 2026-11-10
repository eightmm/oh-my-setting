#!/usr/bin/env python3
"""Advisory file leases: visible, time-limited intent to edit a path.

State is .oms/leases.jsonl, append-only: an `add` row opens a lease, a
`release` row closes it. Expiry is computed when the file is read; nothing is
ever deleted, killed or refused because of a lease. Overlap between two path
patterns reuses the scheduler's conservative test (undecidable means overlap).
Stdlib only; the caller (scripts/lease.sh) holds the repo's file lock around
the mutating verbs.
"""
import argparse
import calendar
import json
import os
import re
import runpy
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import path_scope  # noqa: E402
from oms_graph.scheduler import scopes_overlap  # noqa: E402

DURABLE = runpy.run_path(os.path.join(HERE, "durable-jsonl.py"))
LABEL = "leases.jsonl"
SCHEMA = 1
MIN_TTL, MAX_TTL, DEFAULT_TTL = 60, 8 * 3600, 30 * 60
UNKNOWN = "unknown-session"
# What OMS already puts in a session's or worker's environment, most specific first.
HOLDER_VARS = ("OMS_ATTEMPT_ID", "OMS_ROOM_PARTICIPANT")
STAMP = "%Y-%m-%dT%H:%M:%SZ"


def die(message, code=2):
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(code)


def stamp(epoch):
    return time.strftime(STAMP, time.gmtime(epoch))


def unstamp(text):
    return calendar.timegm(time.strptime(text, STAMP))


def holder():
    """(holder, source env var). Falls back to a labelled unknown; never invents an id."""
    for name in HOLDER_VARS:
        value = os.environ.get(name, "").strip()
        if value and len(value) <= 128 and value.isprintable():
            return value, name
    return UNKNOWN, ""


def parse_ttl(text):
    match = re.fullmatch(r"(\d+)([smh]?)", text.strip())
    if not match:
        die("--ttl must look like 90s, 30m or 2h")
    seconds = int(match.group(1)) * {"": 60, "s": 1, "m": 60, "h": 3600}[match.group(2)]
    if not MIN_TTL <= seconds <= MAX_TTL:
        die("--ttl must be between 1m and 8h")
    return seconds


def repo_pattern(root, raw):
    """Repo-relative pattern for a path or glob given relative to the cwd; refuses anything outside root."""
    if not isinstance(raw, str) or not raw.strip():
        die("empty path")
    parts = raw.replace("\\", "/").split("/")
    static = []
    for index, part in enumerate(parts):
        if path_scope.has_glob(part):
            break
        static.append(part)
    else:
        index = len(parts)
    tail = parts[index:]
    if any(part in ("..", ".") for part in tail):
        die("%s: '.' and '..' are not allowed after a glob character" % raw)
    head = "/".join(static) or ("/" if raw.startswith("/") else ".")
    if not os.path.isabs(head):
        head = os.path.join(os.getcwd(), head)
    head = os.path.realpath(head)
    try:
        inside = os.path.commonpath([head, root]) == root
    except ValueError:
        inside = False
    if not inside:
        die("%s is outside the repository %s" % (raw, root))
    relative = os.path.relpath(head, root)
    pattern = "/".join([relative] + tail) if tail else relative
    if pattern.startswith("./"):
        pattern = pattern[2:]
    try:
        return path_scope.normalize(pattern)
    except ValueError as error:
        die("%s: %s" % (raw, error))


def read_rows(root, store):
    state_root = os.path.dirname(os.path.dirname(store))
    data = DURABLE["read_no_follow"](state_root, store, LABEL, missing_ok=True)
    rows = []
    for number, line in enumerate(data.decode("utf-8", "replace").splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
            ok = isinstance(row, dict) and row.get("op") in ("add", "release") and isinstance(row.get("id"), str)
        except ValueError:
            ok = False
        if not ok:
            die("%s line %d is not a lease row; fix or move the file aside" % (LABEL, number))
        rows.append(row)
    return rows


def leases(rows, now):
    """Every lease with a computed state: active, expired or released, in file order."""
    found = {}
    for row in rows:
        if row["op"] == "add":
            try:
                expires = unstamp(row["expires"])
                paths = [path_scope.normalize(item) for item in row["paths"]]
            except (KeyError, ValueError, TypeError):
                die("%s has an unreadable add row for %s" % (LABEL, row["id"]))
            found[row["id"]] = dict(row, paths=paths, expires_epoch=expires, state="active")
        elif row["id"] in found:
            found[row["id"]]["state"] = "released"
            found[row["id"]]["released_at"] = row.get("ts", "")
    for lease in found.values():
        left = lease["expires_epoch"] - now
        lease["seconds_left"] = max(0, left) if lease["state"] == "active" else 0
        if lease["state"] == "active" and left <= 0:
            lease["state"] = "expired"
    return list(found.values())


def mine(lease, me):
    # Two sessions that both lack an identity cannot be told apart; they count as one holder.
    return lease["holder"] == me


def conflicts(found, patterns, me, shared):
    """Active leases of another holder that overlap `patterns`. Shared leases do not conflict with shared asks."""
    return [lease for lease in found
            if lease["state"] == "active" and not mine(lease, me)
            and not (shared and lease.get("shared"))
            and scopes_overlap(patterns, lease["paths"])]


def left_text(seconds):
    if seconds >= 3600:
        return "%dh%02dm" % (seconds // 3600, seconds % 3600 // 60)
    if seconds >= 60:
        return "%dm" % (seconds // 60)
    return "%ds" % seconds


def describe(lease):
    reason = (' "%s"' % lease["reason"]) if lease.get("reason") else ""
    return "%s %s by %s%s, %s left, paths %s" % (
        lease["id"], "shared" if lease.get("shared") else "exclusive", lease["holder"], reason,
        left_text(lease["seconds_left"]), ", ".join(lease["paths"]))


def append_row(store, row):
    os.makedirs(os.path.dirname(store), exist_ok=True)
    DURABLE["append"](store, (json.dumps(row, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8"), LABEL)


def parser_for(name, usage):
    return argparse.ArgumentParser(prog="oms lease " + name, usage=usage, add_help=False)


def cmd_add(args, root, store):
    ap = parser_for("add", "oms lease add PATH_OR_GLOB... [--ttl 30m] [--reason TEXT] [--shared]")
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--ttl", default="30m")
    ap.add_argument("--reason", default="")
    ap.add_argument("--shared", action="store_true")
    opts = ap.parse_intermixed_args(args)
    ttl = parse_ttl(opts.ttl)
    if len(opts.reason) > 200 or not (opts.reason.isprintable() or not opts.reason):
        die("--reason must be printable text of at most 200 characters")
    patterns = sorted(set(repo_pattern(root, item) for item in opts.paths))
    me, source = holder()
    now = int(time.time())
    clash = conflicts(leases(read_rows(root, store), now), patterns, me, opts.shared)
    row = {"schema": SCHEMA, "op": "add", "id": "l-" + uuid.uuid4().hex[:8], "holder": me,
           "holder_source": source, "paths": patterns, "shared": opts.shared, "reason": opts.reason,
           "ts": stamp(now), "expires": stamp(now + ttl)}
    append_row(store, row)
    print("%s %s until %s (%s) paths %s" % (row["id"], "shared" if opts.shared else "exclusive",
                                           row["expires"], left_text(ttl), ", ".join(patterns)))
    for lease in clash:
        print("warning: overlaps %s" % describe(lease), file=sys.stderr)
    if me == UNKNOWN:
        print("note: no OMS_ATTEMPT_ID or OMS_ROOM_PARTICIPANT; holder recorded as %s" % UNKNOWN, file=sys.stderr)
    return 0


def cmd_list(args, root, store):
    ap = parser_for("list", "oms lease list [--json]")
    ap.add_argument("--json", action="store_true")
    opts = ap.parse_intermixed_args(args)
    found = leases(read_rows(root, store), int(time.time()))
    if opts.json:
        out = [{k: v for k, v in lease.items() if k != "expires_epoch"} for lease in found]
        print(json.dumps(out, sort_keys=True, ensure_ascii=False))
        return 0
    for lease in found:
        if lease["state"] == "released":
            continue
        print("%s %-9s %-9s %-8s %s%s" % (
            lease["id"], lease["state"], "shared" if lease.get("shared") else "exclusive",
            left_text(lease["seconds_left"]) if lease["state"] == "active" else "-",
            lease["holder"] + " ", ", ".join(lease["paths"]) + (("  # " + lease["reason"]) if lease.get("reason") else "")))
    return 0


def staged_paths(root):
    done = subprocess.run(["git", "-C", root, "diff", "--cached", "--name-only", "-z"],
                          capture_output=True, check=False)
    if done.returncode != 0:
        die("git diff --cached failed in %s" % root)
    return [item for item in done.stdout.decode("utf-8", "replace").split("\0") if item]


def cmd_check(args, root, store):
    ap = parser_for("check", "oms lease check PATH... | --staged")
    ap.add_argument("paths", nargs="*")
    ap.add_argument("--staged", action="store_true")
    opts = ap.parse_intermixed_args(args)
    if bool(opts.paths) == opts.staged:
        ap.error("give PATH... or --staged, not both and not neither")
    if opts.staged:
        try:
            patterns = [path_scope.normalize(item) for item in staged_paths(root)]
        except ValueError as error:
            die("staged path: %s" % error)
    else:
        patterns = [repo_pattern(root, item) for item in opts.paths]
    me, _ = holder()
    found = leases(read_rows(root, store), int(time.time()))
    held = False
    for pattern in patterns:
        for lease in found:
            if lease["state"] != "active" or mine(lease, me) or not scopes_overlap([pattern], lease["paths"]):
                continue
            if lease.get("shared"):
                print("shared: %s is under %s" % (pattern, describe(lease)))
            else:
                held = True
                print("held: %s is under %s" % (pattern, describe(lease)))
    if not held:
        print("free: %d path(s) have no exclusive lease held by someone else" % len(patterns))
    return 1 if held else 0


def cmd_release(args, root, store):
    ap = parser_for("release", "oms lease release ID | --mine")
    ap.add_argument("ids", nargs="*")
    ap.add_argument("--mine", action="store_true")
    opts = ap.parse_intermixed_args(args)
    if bool(opts.ids) == opts.mine or len(opts.ids) > 1:
        ap.error("give one ID or --mine")
    me, _ = holder()
    found = {lease["id"]: lease for lease in leases(read_rows(root, store), int(time.time()))}
    if opts.mine:
        targets = [lease for lease in found.values() if lease["state"] != "released" and mine(lease, me)]
    else:
        lease = found.get(opts.ids[0])
        if lease is None or lease["state"] == "released":
            print("no unreleased lease %s" % opts.ids[0], file=sys.stderr)
            return 1
        if lease["state"] == "active" and not mine(lease, me):
            print("lease %s is held by %s; it frees itself at %s" % (lease["id"], lease["holder"], lease["expires"]),
                  file=sys.stderr)
            return 1
        targets = [lease]
    for lease in targets:
        append_row(store, {"schema": SCHEMA, "op": "release", "id": lease["id"], "holder": me, "ts": stamp(int(time.time()))})
        print("released %s" % lease["id"])
    if opts.mine and not targets:
        print("no leases held by %s" % me)
    return 0


COMMANDS = {"add": cmd_add, "list": cmd_list, "check": cmd_check, "release": cmd_release}


def main(argv):
    if len(argv) < 5 or argv[0] != "--root" or argv[2] != "--store" or argv[4] not in COMMANDS:
        print("usage: oms lease add|list|check|release ...", file=sys.stderr)
        return 2
    root = os.path.realpath(argv[1])
    store = os.path.join(os.path.realpath(os.path.dirname(argv[3])), os.path.basename(argv[3]))
    return COMMANDS[argv[4]](argv[5:], root, store)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
