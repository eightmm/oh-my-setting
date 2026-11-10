#!/usr/bin/env python3
"""Read-only inventory of the instruction and memory files provider CLIs keep for one repository.

Names, sizes, counts and modification times only: no file is opened, nothing is written and
nothing here says whether a provider has the feature turned on.
"""

import argparse
import json
import os
import re
import sys
import time

SCHEMA = 1
MAX_WALK = 2000  # entries stat'ed under one directory before the count is reported as capped


def home_dir():
    return os.path.realpath(os.path.expanduser("~"))


def show(path, repo, home):
    """A path the way a person names it: repo-relative, then ~-relative, never a bare home path."""
    real = os.path.abspath(path)
    # The deeper base names the file: a home under the repository is still home.
    for base, prefix in sorted(((repo, ""), (home, "~/")), key=lambda item: -len(item[0] or "")):
        if base and (real == base or real.startswith(base.rstrip("/") + "/")):
            rest = os.path.relpath(real, base)
            return (prefix + rest) if rest != "." else (prefix or ".")
    return "<outside home>/" + os.path.basename(real)


def stamp(seconds):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(seconds))


def file_row(provider, role, path, repo, home):
    try:
        info = os.lstat(path)
    except OSError:
        return None
    row = {"provider": provider, "role": role, "path": show(path, repo, home), "kind": "file"}
    if os.path.islink(path):
        row["link"] = True
        try:
            info = os.stat(path)
        except OSError:
            row.update(kind="broken-link", bytes=0, files=0, modified=None)
            return row
    if os.path.isdir(path):
        return dir_row(provider, role, path, repo, home, row.get("link", False))
    if not os.path.isfile(path):
        return None
    row.update(bytes=info.st_size, files=1, modified=stamp(info.st_mtime))
    return row


def dir_row(provider, role, path, repo, home, link=False):
    if not os.path.isdir(path):
        return None
    files = total = seen = 0
    newest = None
    capped = False
    for base, dirs, names in os.walk(path):
        dirs.sort()
        for name in sorted(names):
            seen += 1
            if seen > MAX_WALK:
                capped = True
                break
            try:
                info = os.stat(os.path.join(base, name))
            except OSError:
                continue
            files += 1
            total += info.st_size
            newest = info.st_mtime if newest is None else max(newest, info.st_mtime)
        if capped:
            break
    row = {"provider": provider, "role": role, "path": show(path, repo, home) + "/", "kind": "directory",
           "bytes": total, "files": files, "modified": stamp(newest) if newest is not None else None}
    if link:
        row["link"] = True
    if capped:
        row["capped"] = True
    return row


def claude_project_key(repo):
    # Claude Code names a project's state directory after its path with separators turned
    # into dashes. That is OMS's reading of the layout, not something the CLI confirmed.
    return re.sub(r"[^A-Za-z0-9]", "-", repo)


def collect(repo):
    home = home_dir()
    codex = os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")
    claude = os.path.join(home, ".claude")
    gemini = os.path.join(home, ".gemini")
    wanted = [
        ("claude", "instructions", os.path.join(claude, "CLAUDE.md")),
        ("claude", "instructions", os.path.join(claude, "rules")),
        ("claude", "instructions", os.path.join(repo, "CLAUDE.md")),
        ("claude", "instructions", os.path.join(repo, ".claude", "CLAUDE.md")),
        ("claude", "instructions", os.path.join(repo, "CLAUDE.local.md")),
        ("claude", "instructions", os.path.join(repo, ".claude", "rules")),
        ("claude", "auto-memory", os.path.join(claude, "projects", claude_project_key(repo), "memory")),
        ("claude", "subagent-memory", os.path.join(claude, "agent-memory")),
        ("claude", "subagent-memory", os.path.join(repo, ".claude", "agent-memory")),
        ("claude", "subagent-memory", os.path.join(repo, ".claude", "agent-memory-local")),
        ("codex", "instructions", os.path.join(codex, "AGENTS.md")),
        ("codex", "instructions", os.path.join(codex, "AGENTS.override.md")),
        ("codex", "instructions", os.path.join(repo, "AGENTS.md")),
        ("codex", "instructions", os.path.join(repo, "AGENTS.override.md")),
        ("codex", "memories", os.path.join(codex, "memories_v2")),
        ("codex", "memories", os.path.join(codex, "memories")),
        ("antigravity", "instructions", os.path.join(gemini, "AGENTS.md")),
        ("antigravity", "instructions", os.path.join(gemini, "GEMINI.md")),
        ("antigravity", "instructions", os.path.join(repo, "GEMINI.md")),
        ("oms", "memory", os.path.join(repo, ".oms", "memory")),
    ]
    rows, seen = [], set()
    for provider, role, path in wanted:
        key = (provider, os.path.abspath(path))
        if key in seen:
            continue
        seen.add(key)
        row = file_row(provider, role, path, repo, home)
        if row is not None:
            rows.append(row)
    return rows


def size(count):
    if count < 1024:
        return "%d B" % count
    if count < 1024 * 1024:
        return "%.1f KB" % (count / 1024)
    return "%.1f MB" % (count / (1024 * 1024))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    repo = os.path.realpath(args.repo)
    rows = collect(repo)
    if args.json:
        json.dump({"schema": SCHEMA, "kind": "oms-memory-sources", "repo": os.path.basename(repo),
                   "contents_read": False, "enabled": "not reported", "sources": rows},
                  sys.stdout, ensure_ascii=False)
        sys.stdout.write("\n")
        return 0
    print("memory sources for %s: %d present; names, sizes and counts only (no file is opened)"
          % (os.path.basename(repo), len(rows)))
    provider = None
    width = max([len(row["path"]) for row in rows] or [0])
    for row in rows:
        if row["provider"] != provider:
            provider = row["provider"]
            print(provider)
        notes = []
        if row["kind"] == "directory":
            notes.append("%d file(s)%s" % (row["files"], "+" if row.get("capped") else ""))
        if row.get("link"):
            notes.append("broken link" if row["kind"] == "broken-link" else "link")
        print("  %-15s  %-*s  %9s  %s%s" % (row["role"], width, row["path"], size(row["bytes"]),
                                           row["modified"] or "-", ("  " + ", ".join(notes)) if notes else ""))
    print("not reported: whether a provider has its memory turned on, and what any file says;"
          " the Claude auto-memory location is derived from the repository path")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
