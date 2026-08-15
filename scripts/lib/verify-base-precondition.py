#!/usr/bin/env python3
"""Name the file a task's verify command needs but the base commit lacks.

Admission re-runs a task's verify command against a worktree whose verification
surface has been restored from the base commit. A verify that names a file only
this task creates therefore cannot be admitted at any point: the floor deletes
the very command it then runs. That verdict otherwise costs a full worker run.

Only high-confidence verifier inputs are reported, and only inside the task's
own declared scope — the deadlock needs the task itself to create the file.
Output operands, redirections, options, assignments, URLs, globs, package
selectors like ./..., bare runner names, and any command that moves the working
directory are left alone: silence here is "no claim", never "checked and fine".

Usage: verify-base-precondition.py REPO VERIFY_COMMAND [ALLOWED_CSV]
Prints one repo-relative path when the precondition is broken, nothing when it
is not. Exit status is always 0; the caller reads the line, not the code.
"""

from __future__ import annotations

import shlex
import subprocess
import sys

OUTPUT_FLAGS = {
    "-o", "--output", "--out", "--outfile", "--report", "--junitxml",
    ">", ">>", "2>", "&>", "tee",
}
# A file the command runs or reads, as opposed to a bare runner name (pytest,
# make) or a data argument.
SCRIPT_SUFFIXES = (
    ".sh", ".bash", ".py", ".js", ".mjs", ".cjs", ".ts", ".rb", ".pl", ".lua",
    ".jl", ".exs", ".go",
)
UNSAFE_CHARS = "*?[]$`\\<>|"


def normalize(value: str) -> str:
    value = value.replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    return value.rstrip("/")


def in_scope(path: str, allowed: list[str]) -> bool:
    for prefix in allowed:
        prefix = normalize(prefix)
        if prefix in ("", "."):
            return True
        if path == prefix or path.startswith(prefix + "/"):
            return True
    return False


def missing_path(repo: str, command: str, allowed: list[str]) -> str:
    try:
        words = shlex.split(command)
    except ValueError:
        return ""  # unparseable quoting is not evidence of anything
    if any(word in ("cd", "pushd") for word in words):
        return ""  # the paths are relative to a directory this cannot resolve
    skip_next = False
    for word in words:
        if skip_next:
            skip_next = False
            continue
        if word in OUTPUT_FLAGS:
            skip_next = True
            continue
        if word.startswith("-") or "=" in word or "://" in word:
            continue
        path = normalize(word.split("::", 1)[0])  # pytest node ids name a case
        if not path or path.startswith(("/", "..", "~")):
            continue
        if "..." in path or any(char in path for char in UNSAFE_CHARS):
            continue
        if "/" not in path and not path.endswith(SCRIPT_SUFFIXES):
            continue  # a bare runner name is not a path claim
        if not in_scope(path, allowed):
            continue
        if subprocess.call(
            ["git", "-C", repo, "cat-file", "-e", "HEAD:" + path],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        ) != 0:
            return path
    return ""


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        sys.stderr.write("usage: verify-base-precondition.py REPO VERIFY [ALLOWED_CSV]\n")
        return 2
    repo, command = argv[1], argv[2]
    allowed = [p.strip() for p in (argv[3] if len(argv) > 3 else "").split(",") if p.strip()]
    found = missing_path(repo, command, allowed)
    if found:
        print(found)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
