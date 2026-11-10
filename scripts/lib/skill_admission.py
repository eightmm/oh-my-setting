#!/usr/bin/env python3
"""Admission record for project skills: who admitted what, from where, with which bytes.

`row` prints one JSONL admission row for .oms/skills/<name>; the caller appends it
through durable-jsonl.py under the ledger lock. `check` compares every local
project skill with its last row and prints warnings only. Imported bundles keep
their own digest pin in .oms/skill-store/<name>/lock.json and are not repeated here.
"""

import argparse
import getpass
import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

LEDGER = Path(".oms") / "skill-admissions.jsonl"
SKILLS = Path(".oms") / "skills"


def repo_root(raw):
    start = Path(raw).resolve()
    try:
        out = subprocess.run(
            ["git", "-C", str(start), "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        out = ""
    return Path(out).resolve() if out else start


def digests(skill_dir):
    """SKILL.md digest and one digest over the whole file set (path + content)."""
    skill_md = hashlib.sha256((skill_dir / "SKILL.md").read_bytes()).hexdigest()
    whole = hashlib.sha256()
    for current, dirs, files in os.walk(str(skill_dir)):
        dirs.sort()
        for name in sorted(files + [d for d in dirs if os.path.islink(os.path.join(current, d))]):
            path = Path(current) / name
            rel = path.relative_to(skill_dir).as_posix()
            if path.is_symlink():
                body = b"link:" + os.readlink(str(path)).encode("utf-8", "surrogateescape")
            else:
                body = path.read_bytes()
            whole.update(rel.encode("utf-8", "surrogateescape") + b"\0")
            whole.update(hashlib.sha256(body).hexdigest().encode("ascii") + b"\n")
    return skill_md, whole.hexdigest()


def admitted_by():
    try:
        name = subprocess.run(
            ["git", "config", "user.name"], capture_output=True, text=True, check=False
        ).stdout.strip()
    except OSError:
        name = ""
    if name:
        return name
    try:
        return getpass.getuser()
    except Exception:  # noqa: BLE001 - no login name is not an error here
        return "unknown"


def command_row(args):
    root = repo_root(args.repo)
    skill_dir = root / SKILLS / args.name
    if not (skill_dir / "SKILL.md").is_file():
        print("error: no project skill: %s" % args.name, file=sys.stderr)
        return 2
    skill_md, files = digests(skill_dir)
    row = {
        "schema": 1,
        "name": args.name,
        "source": args.source,
        "skill_sha256": skill_md,
        "files_sha256": files,
        "admitted_by": admitted_by(),
        "ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }
    print(json.dumps(row, sort_keys=True, ensure_ascii=False, separators=(",", ":")))
    return 0


def last_rows(root):
    rows = {}
    skipped = 0
    path = root / LEDGER
    if not path.is_file():
        return rows, skipped
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                skipped += 1
                continue
            if isinstance(row, dict) and isinstance(row.get("name"), str):
                rows[row["name"]] = row
    return rows, skipped


def command_check(args):
    root = repo_root(args.repo)
    base = root / SKILLS
    if not base.is_dir():
        return 0
    rows, skipped = last_rows(root)
    seen = 0
    for skill_dir in sorted(p for p in base.iterdir() if p.is_dir() and (p / "SKILL.md").is_file()):
        seen += 1
        name = skill_dir.name
        row = rows.get(name)
        if row is None:
            print("warn: skill %s unrecorded: no admission row; run 'oms skill-forge admit %s' once you have reviewed it"
                  % (name, name))
            continue
        try:
            skill_md, files = digests(skill_dir)
        except OSError as exc:
            print("warn: skill %s could not be read for the admission check: %s" % (name, exc))
            continue
        if skill_md != row.get("skill_sha256") or files != row.get("files_sha256"):
            what = "SKILL.md" if skill_md != row.get("skill_sha256") else "files other than SKILL.md"
            print("warn: skill %s changed since admission (%s differ; admitted %s by %s from %s); review it, then 'oms skill-forge admit %s'"
                  % (name, what, row.get("ts", "?"), row.get("admitted_by", "?"), row.get("source", "?"), name))
    if skipped:
        print("note: %d unreadable row(s) skipped in %s" % (skipped, LEDGER))
    if seen:
        print("note: %d local project skill(s) checked against %s" % (seen, LEDGER))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    row = sub.add_parser("row")
    row.add_argument("--repo", default=".")
    row.add_argument("--name", required=True)
    row.add_argument("--source", required=True)
    row.set_defaults(func=command_row)
    check = sub.add_parser("check")
    check.add_argument("--repo", default=".")
    check.set_defaults(func=command_check)
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
