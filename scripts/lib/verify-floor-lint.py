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

import re
import shlex
import sys

FLOOR_READERS = {
    "grep", "egrep", "fgrep", "rg", "sed", "awk", "cat", "head", "tail",
    "diff", "cmp", "sort", "wc", "od", "strings", "cut", "tr", "uniq",
    "jq", "sha256sum", "md5sum", "shasum", "cksum", "comm", "join", "paste",
    "nl", "tac", "rev", "xxd", "hexdump", "base64",
    # The reviewer's blocking finding: the nearest siblings inside the very
    # families the sweep designated still admitted — one token away from a
    # blocked command reproduces the dead-park state.
    "sha1sum", "sha224sum", "sha384sum", "sha512sum", "b2sum", "sum",
    "base32", "basenc",
}


def floor_path_spelling(value):
    """Normalize only Bash's unambiguous repo-relative ./ spelling."""
    while value.startswith("./"):
        value = value[2:]
    return value


def unwrap_strict_command_wrapper(words):
    """Unwrap one deterministic command/env layer, leaving unknown forms alone."""
    if not words:
        return words
    head = words[0]
    if head == "command":
        if len(words) < 2 or words[1] in ("-v", "-V"):
            return words
        index = 1
        if words[index] in ("-p", "--"):
            index += 1
        if index >= len(words) or words[index].startswith("-"):
            return words
        return words[index:]
    if head in ("env", "/bin/env", "/usr/bin/env"):
        index = 1
        if index < len(words) and words[index] == "--":
            index += 1
        elif index < len(words) and words[index].startswith("-"):
            return words
        while index < len(words) and re.fullmatch(
            r"[A-Za-z_][A-Za-z0-9_]*=.*", words[index]
        ):
            index += 1
        if index >= len(words) or words[index].startswith("-"):
            return words
        return words[index:]
    return words


def floor_incompatible_reads(verify, allowed_paths):
    try:
        tokens = shlex.split(verify)
    except ValueError:
        return []
    # Shell style writes "cmd path; next" with the separator attached to the
    # path token, and an attached separator defeated the exact match (field
    # finding, 2026-08-19: a planner-authored `grep ... smoke.sh; done`
    # admitted). Split trailing/leading separator punctuation into their own
    # boundary tokens before matching.
    split_tokens = []
    for token_value in tokens:
        # A token with whitespace is a quoted argument (a bash -c script) —
        # it must stay intact for the recursion below to see it whole.
        if any(ch.isspace() for ch in token_value):
            split_tokens.append(token_value)
            continue
        parts = re.split(r"([;|&]+)", token_value)
        for part in parts:
            if part:
                split_tokens.append(part)
    tokens = split_tokens
    boundary = {"&&", "||", ";", "|", "&", ";;"}
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
    allowed = {floor_path_spelling(path) for path in allowed_paths}
    for command in commands:
        if not command:
            continue
        words = list(command)
        while words and "=" in words[0] and "/" not in words[0].split("=", 1)[0]:
            words = words[1:]
        # Compound keywords put themselves ahead of the real command head
        # (`do grep ...`, `if grep ...`) — same field finding as the attached
        # separators: shell style, not evasion, but it shielded the reader.
        compound = {"do", "then", "else", "elif", "if", "while", "until", "time", "!", "{"}
        while words and words[0] in compound:
            words = words[1:]
        if not words:
            continue
        # `command` and `env` commonly wrap a verifier without changing which
        # executable reads the file. Unwrap only the one-layer forms whose
        # shell meaning is certain; option-rich or nested forms remain unknown
        # and rely on the runtime floor instead of risking a false rejection.
        words = unwrap_strict_command_wrapper(words)
        # bash -c 'inner': the inner string is where the field defect lived.
        if words[0].rsplit("/", 1)[-1] in ("bash", "sh", "zsh") and "-c" in words[:3]:
            marker = words.index("-c")
            if marker + 1 < len(words):
                hits.extend(floor_incompatible_reads(words[marker + 1], allowed_paths))
            continue
        head_word = words[0].rsplit("/", 1)[-1]
        for index_w, word in enumerate(words):
            if word == "<" and index_w + 1 < len(words):
                target = floor_path_spelling(words[index_w + 1])
                if target in allowed:
                    hits.append((target, "stdin redirect"))
            elif word.startswith("<") and len(word) > 1:
                target = floor_path_spelling(word[1:])
                if target in allowed:
                    hits.append((target, "stdin redirect"))
        if head_word in FLOOR_READERS:
            for word in words[1:]:
                target = floor_path_spelling(word)
                if target in allowed:
                    hits.append((target, head_word))
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
