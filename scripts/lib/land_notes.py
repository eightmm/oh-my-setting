#!/usr/bin/env python3
"""`oms land notes --from REV --to REV`: a deterministic skeleton for release notes.

Reads the first-parent commit subjects of one range and the land receipts of
this checkout, groups the subjects by their Conventional Commit type and marks
the commits a land receipt recorded. It writes nothing but the skeleton (to
stdout, or to a new file with --output), creates no tag, release or commit, and
calls no model: the wording of the notes is left to a person.
"""

import argparse
import json
import os
import re
import subprocess
import sys

LIB = os.path.dirname(os.path.abspath(__file__))
SUBJECT = re.compile(r"^([a-z]+)(\([A-Za-z0-9._/-]+\))?(!)?: (.+)$")
ORDER = ("feat", "fix", "perf", "refactor", "docs", "test", "build", "ci", "chore", "revert")
MAX_COMMITS = 500


def stop(message, code=2):
    print("land notes: " + message, file=sys.stderr)
    raise SystemExit(code)


def git(repo, *args):
    done = subprocess.run(["git", "-C", repo] + list(args), capture_output=True, text=True,
                          check=False, stdin=subprocess.DEVNULL)
    return done.stdout if done.returncode == 0 else None


def resolve(repo, name, label):
    if not name or name.startswith("-"):
        stop("%s needs a commit" % label)
    out = git(repo, "rev-parse", "--verify", "--quiet", name + "^{commit}")
    if not out:
        stop("%s does not name a commit in this repository" % label)
    return out.strip()


def one_line(text, limit=200):
    text = " ".join("".join(ch if ch.isprintable() else " " for ch in text).split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def land_states(repo):
    """{sha: state} from this checkout's land receipts; empty when the receipt directory cannot be read."""
    script = '. "$1/land-receipts.sh"; oms_land_state_dir "$2"'
    try:
        done = subprocess.run(["bash", "-c", script, "land-notes", LIB, repo], capture_output=True,
                              text=True, timeout=20, check=False, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    directory = done.stdout.strip()
    if done.returncode != 0 or not directory:
        return None
    states = {}
    try:
        names = sorted(os.listdir(directory))
    except OSError:
        return states
    for name in names:
        path = os.path.join(directory, name)
        if not name.endswith(".json") or os.path.islink(path) or not os.path.isfile(path):
            continue
        try:
            with open(path, encoding="utf-8") as handle:
                row = json.load(handle)
        except (OSError, ValueError):
            continue
        sha, state = row.get("sha"), row.get("state")
        if isinstance(sha, str) and isinstance(state, str):
            # A passed receipt for a commit outlives a later failed retry of the same commit.
            if states.get(sha) != "passed":
                states[sha] = one_line(state, 20)
    return states


def main(argv):
    parser = argparse.ArgumentParser(prog="land.sh notes", add_help=False)
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--from", dest="start", required=True)
    parser.add_argument("--to", dest="end", required=True)
    parser.add_argument("--output")
    try:
        args = parser.parse_args(argv)
    except SystemExit:
        stop("usage: land.sh notes [--repo PATH] --from REV --to REV [--output NEW_FILE]")
    repo = os.path.realpath(args.repo)
    start, end = resolve(repo, args.start, "--from"), resolve(repo, args.end, "--to")
    if git(repo, "merge-base", "--is-ancestor", start, end) is None:
        stop("--from is not an ancestor of --to; a notes range runs forward along one history")
    log = git(repo, "log", "--first-parent", "--reverse", "--format=%H%x09%s", "%s..%s" % (start, end))
    if log is None:
        stop("the range could not be read", 3)
    commits = [line.split("\t", 1) for line in log.splitlines() if "\t" in line]
    states = land_states(repo)
    groups, breaking = {}, []
    for sha, subject in commits[:MAX_COMMITS]:
        match = SUBJECT.match(subject)
        kind = match.group(1) if match and match.group(1) in ORDER else "other"
        text = one_line(match.group(4) if match and kind != "other" else subject)
        scope = match.group(2) if match and kind != "other" and match.group(2) else ""
        mark = ""
        if states and sha in states:
            mark = " [land: %s]" % states[sha]
        entry = "- %s%s (%s)%s" % (scope[1:-1] + ": " if scope else "", text, sha[:7], mark)
        groups.setdefault(kind, []).append(entry)
        if match and match.group(3):
            breaking.append(entry)
    lines = ["# Notes for %s..%s" % (start[:7], end[:7]), "",
             "<!-- Skeleton from commit subjects and land receipts; a person writes the notes. "
             "Nothing was tagged, released or committed. -->", "",
             "%d commit(s) on the first-parent line%s." % (
                 len(commits), "; only the first %d are listed" % MAX_COMMITS if len(commits) > MAX_COMMITS else "")]
    if states is None:
        lines.append("Land receipts: not read (the receipt directory could not be resolved).")
    else:
        landed = sum(1 for sha, _ in commits if states.get(sha) == "passed")
        lines.append("Land receipts on this machine: %d of these commits were the head of a passed land; "
                     "a commit with no mark was not landed from here or landed inside a later one." % landed)
    if breaking:
        lines += ["", "## Breaking (marked `!`)"] + breaking
    for kind in ORDER + ("other",):
        if groups.get(kind):
            lines += ["", "## %s" % kind] + groups[kind]
    text = "\n".join(lines) + "\n"
    if not args.output:
        sys.stdout.write(text)
        return 0
    try:
        handle = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o644)
    except FileExistsError:
        stop("--output refuses to replace %s; name a new file" % args.output)
    except OSError as error:
        stop("--output could not be created: %s" % error.strerror, 3)
    with os.fdopen(handle, "w", encoding="utf-8") as out:
        out.write(text)
    print("land notes: wrote %d line(s) to %s" % (len(lines), args.output))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
