#!/usr/bin/env python3
"""Read-only audit of training-run checkpoint directories.

Counts checkpoint files per run directory with stat only (contents are never
opened), compares each directory with the default "keep only latest and best",
and prints a warning where files pile up. Nothing is ever deleted.
"""
import argparse
import json
import os
import sys

EXTENSIONS = (".pt", ".pth", ".ckpt", ".safetensors", ".bin", ".h5", ".keras", ".msgpack")
DEFAULT_NAMES = ("latest", "best")
DEFAULT_MAX = 2
MAX_DIRS = 2000
MAX_FILES = 50000
FIELD = "- Checkpoints:"

EPILOG = """\
A run directory is any directory that directly contains checkpoint files.
Checkpoint files are regular files with the extensions: %s.
Symlinks are never followed. A directory follows the default when every
checkpoint file has the stem "latest" or "best" and there are at most --max
files (default %d). A project that opted in to keeping more can raise --max.
With no DIR, scans checkpoints/ under the repository plus any directory named
on the "- Checkpoints:" line of PROJECT.md. The walk stops at %d directories or
%d checkpoint files and says so. Exit: 0 normally; 1 with --strict when any
directory warns; 2 on bad usage. Nothing is deleted and file contents are
never read.
""" % (", ".join(EXTENSIONS), DEFAULT_MAX, MAX_DIRS, MAX_FILES)


def human(size):
    value = float(size)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < 1024 or unit == "TiB":
            return "%d B" % size if unit == "B" else "%.1f %s" % (value, unit)
        value /= 1024.0
    return "%d B" % size


def project_dirs(repo):
    """Directories named on a '- Checkpoints:' line of PROJECT.md, if any."""
    path = os.path.join(repo, "PROJECT.md")
    found = []
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                text = line.strip()
                if not text.startswith(FIELD):
                    continue
                value = text[len(FIELD):]
                for part in value.replace(";", ",").split(","):
                    part = part.strip().strip("`").strip()
                    if part:
                        found.append(part)
    except OSError:
        pass
    return found


def show(path, repo):
    try:
        rel = os.path.relpath(path, repo)
    except ValueError:
        return path
    return path if rel.startswith("..") else rel


def scan(roots, limits):
    runs = []
    seen = set()
    state = {"dirs": 0, "files": 0, "dirs_hit": False, "files_hit": False}
    stack = list(reversed(roots))
    while stack:
        current = stack.pop()
        if state["dirs"] >= limits[0]:
            state["dirs_hit"] = True
            break
        state["dirs"] += 1
        real = os.path.realpath(current)
        if real in seen:
            continue
        seen.add(real)
        files = []
        subdirs = []
        try:
            with os.scandir(current) as entries:
                for entry in entries:
                    try:
                        if entry.is_symlink():
                            continue
                        if entry.is_dir(follow_symlinks=False):
                            subdirs.append(entry.path)
                        elif entry.is_file(follow_symlinks=False) and entry.name.lower().endswith(EXTENSIONS):
                            files.append((entry.name, entry.stat(follow_symlinks=False).st_size))
                    except OSError:
                        continue
        except OSError:
            continue
        room = limits[1] - state["files"]
        if len(files) > room:
            files = sorted(files)[:max(room, 0)]
            state["files_hit"] = True
        state["files"] += len(files)
        if files:
            runs.append((current, sorted(files)))
        if state["files_hit"]:
            break
        stack.extend(sorted(subdirs, reverse=True))
    return runs, state


def stem(name):
    return os.path.splitext(name)[0].lower()


def judge(files, max_files):
    names_ok = all(stem(n) in DEFAULT_NAMES for n, _ in files)
    count_ok = len(files) <= max_files
    return names_ok, count_ok


def main(argv):
    parser = argparse.ArgumentParser(
        prog="oms run checkpoints",
        description="Report training-run directories that pile up checkpoint files "
                    "(read-only; OMS never deletes checkpoints).",
        epilog=EPILOG,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("dirs", nargs="*", metavar="DIR", help="directories to scan")
    parser.add_argument("--max", type=int, default=DEFAULT_MAX, dest="max_files",
                        help="most checkpoint files a run directory may hold (default %d)" % DEFAULT_MAX)
    parser.add_argument("--strict", action="store_true", help="exit 1 when any directory warns")
    parser.add_argument("--json", action="store_true", help="print JSON (schema 1, kind oms-run-checkpoints)")
    parser.add_argument("--repo", default=".", help=argparse.SUPPRESS)
    args = parser.parse_intermixed_args(argv)
    if args.max_files < 0:
        parser.error("--max takes a whole number >= 0")

    repo = os.path.realpath(args.repo)
    notes = []
    roots = []
    project_field = None
    if args.dirs:
        for given in args.dirs:
            full = given if os.path.isabs(given) else os.path.join(os.getcwd(), given)
            if os.path.islink(full):
                parser.error("%s is a symlink; symlinks are not followed" % given)
            if not os.path.isdir(full):
                parser.error("not a directory: %s" % given)
            roots.append(os.path.realpath(full))
    else:
        candidates = [os.path.join(repo, "checkpoints")]
        named = project_dirs(repo)
        project_field = bool(named)
        for item in named:
            candidates.append(item if os.path.isabs(item) else os.path.join(repo, item))
        if not named:
            notes.append('PROJECT.md has no "- Checkpoints:" value; scanned checkpoints/ only')
        for candidate in candidates:
            if os.path.islink(candidate):
                notes.append("skipped a symlink: %s" % show(candidate, repo))
            elif os.path.isdir(candidate):
                roots.append(os.path.realpath(candidate))
        if not roots:
            notes.append("no checkpoint directory found")

    runs, state = scan(roots, (MAX_DIRS, MAX_FILES))
    if state["dirs_hit"]:
        notes.append("scan stopped after %d directories; later ones were not counted" % MAX_DIRS)
    if state["files_hit"]:
        notes.append("scan stopped after %d checkpoint files; later ones were not counted" % MAX_FILES)

    rows = []
    for path, files in runs:
        names_ok, count_ok = judge(files, args.max_files)
        total = sum(size for _, size in files)
        offenders = [n for n, _ in files if stem(n) not in DEFAULT_NAMES]
        rows.append({
            "path": show(path, repo),
            "files": len(files),
            "bytes": total,
            "follows_default": names_ok and count_ok,
            "examples": (offenders or [n for n, _ in files])[:3],
            "per_epoch_names": not names_ok,
        })

    warned = [row for row in rows if not row["follows_default"]]
    total_files = sum(row["files"] for row in rows)
    total_bytes = sum(row["bytes"] for row in rows)

    if args.json:
        print(json.dumps({
            "schema": 1,
            "kind": "oms-run-checkpoints",
            "max": args.max_files,
            "extensions": list(EXTENSIONS),
            "project_checkpoints_field": project_field,
            "runs": rows,
            "totals": {"run_dirs": len(rows), "files": total_files,
                       "bytes": total_bytes, "warnings": len(warned)},
            "limits": {"dirs_hit": state["dirs_hit"], "files_hit": state["files_hit"]},
            "notes": notes,
            "deleted": 0,
        }, indent=2, sort_keys=True))
    else:
        for row in rows:
            size = human(row["bytes"])
            if row["follows_default"]:
                print("ok: %s  %d file%s  %s  follows the default (latest + best)" % (
                    row["path"], row["files"], "" if row["files"] == 1 else "s", size))
                continue
            if row["per_epoch_names"]:
                why = "names like %s suggest per-epoch saves" % ", ".join(row["examples"])
            else:
                why = "%d files exceed --max %d" % (row["files"], args.max_files)
            print("warn: %s holds %d checkpoint files (%s); the default is latest + best; %s; "
                  "a project that opted in to keep more can raise --max" % (
                      row["path"], row["files"], size, why))
        for note in notes:
            print("note: %s" % note)
        print("summary: %d run director%s, %d checkpoint files, %s, %d warning%s; nothing was deleted" % (
            len(rows), "y" if len(rows) == 1 else "ies", total_files, human(total_bytes),
            len(warned), "" if len(warned) == 1 else "s"))

    return 1 if (args.strict and warned) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
