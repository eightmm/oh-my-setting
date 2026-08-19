#!/usr/bin/env python3
"""Content reads of task-modified files inside a verify command.

The admission gate's base floor re-runs a verify with the verifier restored
from HEAD, so a verify that READS the content of a file the task modifies
(grep/sed/cat/redirect of an allowed path) can never pass it — the content it
looks for only exists in the patch. EXECUTING the restored file (bash/pytest
of the suite) is the floor's whole point and stays admissible. Unparseable
structures admit: the runtime floor is the backstop, and a false reject
blocks planning while a false admit is still caught at run time
(three-family council consensus, 2026-08-18).

Single source of truth: agent-plan.sh's admission gate loads this file via
runpy (the plan-receipt.py idiom), and `agent-plan lint-verify` runs it as a
standalone front door so a spec author can lint an acceptance or verify
before a planner ever copies it.
"""

import shlex
import sys

FLOOR_READERS = {
    "grep", "egrep", "fgrep", "rg", "sed", "awk", "cat", "head", "tail",
    "diff", "cmp", "sort", "wc", "od", "strings", "cut", "tr", "uniq",
}


def floor_incompatible_reads(verify, allowed_paths):
    try:
        tokens = shlex.split(verify)
    except ValueError:
        return []
    boundary = {"&&", "||", ";", "|", "&"}
    commands = []
    current = []
    for token in tokens:
        if token in boundary:
            commands.append(current)
            current = []
        else:
            current.append(token)
    commands.append(current)
    hits = []
    allowed = set(allowed_paths)
    for command in commands:
        if not command:
            continue
        # bash -c 'inner': the inner string is where the field defect lived.
        if command[0].rsplit("/", 1)[-1] in ("bash", "sh", "zsh") and "-c" in command[:3]:
            marker = command.index("-c")
            if marker + 1 < len(command):
                hits.extend(floor_incompatible_reads(command[marker + 1], allowed_paths))
            continue
        words = list(command)
        while words and "=" in words[0] and "/" not in words[0].split("=", 1)[0]:
            words = words[1:]
        if not words:
            continue
        head_word = words[0].rsplit("/", 1)[-1]
        for index_w, word in enumerate(words):
            if word == "<" and index_w + 1 < len(words) and words[index_w + 1] in allowed:
                hits.append((words[index_w + 1], "stdin redirect"))
            elif word.startswith("<") and len(word) > 1 and word[1:] in allowed:
                hits.append((word[1:], "stdin redirect"))
        if head_word in FLOOR_READERS:
            for word in words[1:]:
                if word in allowed:
                    hits.append((word, head_word))
    return hits


def main(argv):
    import argparse

    parser = argparse.ArgumentParser(
        description="Lint a verify/acceptance command against the admission floor."
    )
    parser.add_argument("--verify", required=True, help="the command to lint")
    parser.add_argument(
        "--allowed",
        required=True,
        help="comma-separated repo-relative paths the work may modify",
    )
    args = parser.parse_args(argv)
    allowed = [part for part in args.allowed.split(",") if part]
    hits = floor_incompatible_reads(args.verify, allowed)
    if not hits:
        print("lint-verify: ok (no content reads of allowed paths)")
        return 0
    for target, reader in hits:
        print("floor_incompatible_verifier: %s (%s)" % (target, reader))
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
