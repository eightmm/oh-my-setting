#!/usr/bin/env python3
"""Receipts for external science tools: .oms/external.jsonl.

One append-only JSONL file (last row for an id wins). A receipt records what
was used (kind, name, version or commit, sha256, license text as declared,
environment lock file) and whether using it can send data out (egress). The
license is recorded, never judged; egress is the tool provider's own
description, so "none" is a declaration and not a network observation.
Stdlib only.
"""
import argparse
import datetime
import hashlib
import json
import os
import re
import sys

KINDS = ("skill", "mcp", "model", "dataset", "tool")
EGRESS = ("none", "public-query", "uploads-input")
EGRESS_NOTE = {
    "none": "declared as not sending data out (a declaration, not observed)",
    "public-query": "queries go to an external provider; unpublished structures or sequences "
                    "in a query leave this machine",
    "uploads-input": "input files are uploaded to an external provider; unpublished data "
                     "leaves this machine",
}
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/@:+-]{0,127}$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def die(message, code=2):
    print("error: %s" % message, file=sys.stderr)
    sys.exit(code)


def store_path(root):
    return os.environ.get("OMS_EXTERNAL_FILE") or os.path.join(root, ".oms", "external.jsonl")


def file_sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load(path):
    rows, order = {}, []
    if not os.path.exists(path):
        return rows, order
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                die("%s line %d is not JSON" % (path, number))
            if not isinstance(row, dict) or not isinstance(row.get("id"), str):
                die("%s line %d has no id" % (path, number))
            if row["id"] not in rows:
                order.append(row["id"])
            rows[row["id"]] = row
    return rows, order


def receipt_id(kind, name, version):
    seed = "\0".join((kind, name, version)).encode("utf-8")
    return "ext-" + hashlib.sha256(seed).hexdigest()[:10]


def rel(root, path):
    absolute = os.path.realpath(path)
    base = os.path.realpath(root)
    return os.path.relpath(absolute, base) if absolute.startswith(base + os.sep) else absolute


def describe_egress(row):
    return "egress %s: %s" % (row["egress"], EGRESS_NOTE.get(row["egress"], "unknown value"))


def cmd_add(args, root, path):
    if not NAME_RE.match(args.name):
        die("name must match %s" % NAME_RE.pattern)
    version = args.version or ""
    if args.egress != "none" and not args.approver:
        die("egress %s needs --approver (who accepted that data may leave); not recorded as verified"
            % args.egress)
    row = {"id": receipt_id(args.kind, args.name, version), "kind": args.kind, "name": args.name,
           "version": version, "license": args.license or "unknown", "egress": args.egress,
           "approver": args.approver or "", "added_at": now()}
    if args.sha256:
        if not SHA_RE.match(args.sha256):
            die("--sha256 must be 64 lowercase hex characters")
        row["sha256"] = args.sha256
    if args.file:
        if not os.path.isfile(args.file):
            die("file not found: %s" % args.file)
        digest = file_sha256(args.file)
        if args.sha256 and args.sha256 != digest:
            die("--sha256 does not match %s (%s)" % (args.file, digest))
        row["sha256"] = digest
        row["file"] = rel(root, args.file)
    if args.env:
        if not os.path.isfile(args.env):
            die("env lock not found: %s" % args.env)
        row["env_lock"] = rel(root, args.env)
        row["env_lock_sha256"] = file_sha256(args.env)
    if args.note:
        row["note"] = args.note
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
    print("%s: %s %s%s recorded" % (row["id"], row["kind"], row["name"],
                                     "@" + version if version else ""))
    print("license: %s (recorded as declared; OMS does not judge licenses)" % row["license"])
    print(describe_egress(row))
    if not version and "sha256" not in row:
        print("note: no version, commit or sha256; this receipt cannot pin what was used")
    if not row.get("env_lock"):
        print("note: no environment lock recorded (--env PATH)")
    return 0


def cmd_list(args, root, path):
    rows, order = load(path)
    if args.json:
        print(json.dumps([rows[i] for i in order], ensure_ascii=False, indent=2))
        return 0
    if not order:
        print("no external receipts (%s)" % path)
        return 0
    for ident in order:
        row = rows[ident]
        print("%s  %-7s %s%s | license %s | egress %s" % (
            ident, row["kind"], row["name"], "@" + row["version"] if row.get("version") else "",
            row.get("license", "unknown"), row.get("egress", "?")))
    return 0


def cmd_show(args, root, path):
    rows, _ = load(path)
    row = rows.get(args.id)
    if row is None:
        die("no such receipt: %s" % args.id)
    print(json.dumps(row, ensure_ascii=False, indent=2, sort_keys=True))
    print(describe_egress(row))
    return 0


def cmd_check(args, root, path):
    """Recompute recorded hashes for files that still exist. Exit 1 on drift."""
    rows, order = load(path)
    ids = [args.id] if args.id else order
    bad = 0
    for ident in ids:
        row = rows.get(ident)
        if row is None:
            die("no such receipt: %s" % ident)
        findings = []
        for key, hkey in (("file", "sha256"), ("env_lock", "env_lock_sha256")):
            target = row.get(key)
            if not target or hkey not in row:
                continue
            location = target if os.path.isabs(target) else os.path.join(root, target)
            if not os.path.isfile(location):
                findings.append("%s missing: %s" % (key, target))
            elif file_sha256(location) != row[hkey]:
                findings.append("%s changed: %s" % (key, target))
        if row.get("egress") != "none":
            findings.append("note: " + describe_egress(row))
        drift = [f for f in findings if not f.startswith("note: ")]
        bad += 1 if drift else 0
        print("%s %s: %s" % ("drift" if drift else "ok   ", ident, "; ".join(findings) or "hashes match"))
    return 1 if bad else 0


def main(argv):
    parser = argparse.ArgumentParser(prog="external")
    parser.add_argument("--root", required=True)
    sub = parser.add_subparsers(dest="cmd", required=True)
    add = sub.add_parser("add")
    add.add_argument("--kind", required=True, choices=KINDS)
    add.add_argument("--name", required=True)
    add.add_argument("--version")
    add.add_argument("--sha256")
    add.add_argument("--file")
    add.add_argument("--license")
    add.add_argument("--env")
    add.add_argument("--egress", choices=EGRESS, default="none")
    add.add_argument("--approver")
    add.add_argument("--note")
    lst = sub.add_parser("list")
    lst.add_argument("--json", action="store_true")
    show = sub.add_parser("show")
    show.add_argument("id")
    chk = sub.add_parser("check")
    chk.add_argument("id", nargs="?")
    args = parser.parse_args(argv)
    path = store_path(args.root)
    return {"add": cmd_add, "list": cmd_list, "show": cmd_show, "check": cmd_check}[args.cmd](
        args, args.root, path)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
