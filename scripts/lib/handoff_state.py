#!/usr/bin/env python3
"""Mechanical handoff state: the fields of a handoff digest that .oms already records.

`session-handoff.sh capture` used to leave plan status, the last decisions, the
changed files and the last failing check to whatever the model wrote in its
final message. Each of them has a recorded source, so they are read from it
here, in a fixed order, and printed ahead of any model-written summary. No model
is called and nothing is written or run: it reads the plan file, the task packet,
the failure ledger and `git status`. A source with nothing recorded prints "not
recorded"; a reader that failed prints "unavailable", so absence is never read as
success. The whole block is held under CAP bytes with a `truncated:` marker.
"""

import json
import os
import re
import subprocess
import sys

CAP = 1536
PACKET_LIMIT = 64 * 1024
FILES, DECISIONS = 8, 3
# Same test handoff_decisions.py applies to each stripped line of the section.
STAMPED = re.compile(r"^- (\d{4}-\d{2}-\d{2})T\S*Z \[[^\]]*\] (.+)$")
SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def one_line(value, limit):
    text = " ".join("".join(ch if ch.isprintable() else " " for ch in str(value)).split())
    raw = text.encode("utf-8")
    if len(raw) > limit:
        text = raw[:limit - 3].decode("utf-8", errors="ignore") + "..."
    return text


def packet_sections(repo):
    path = os.path.join(repo, ".oms", "task", "current.md")
    try:
        if any(os.path.islink(p) for p in (path, os.path.dirname(path))):
            return {}
        with open(path, "rb") as handle:
            raw = handle.read(PACKET_LIMIT + 1)
        if len(raw) > PACKET_LIMIT:
            return {}
        text = raw.decode("utf-8")
    except (OSError, ValueError):
        return {}
    return {name: body for name, body in re.findall(r"^## ([^\n]+)\n(.*?)(?=^## |\Z)", text, re.M | re.S)}


def bullets(body):
    return [line[2:].strip() for line in body.splitlines() if line.startswith("- ") and line[2:].strip()]


def plan_line(repo):
    path = os.path.join(repo, ".oms", "plan", "tasks.json")
    if not os.path.isfile(path) or os.path.islink(path):
        return "- plan: not recorded"
    try:
        with open(path, encoding="utf-8") as handle:
            plan = json.load(handle)
        tasks = plan.get("tasks") or {}
        rows = list(tasks.values()) if isinstance(tasks, dict) else list(tasks)
        counts = {}
        for row in rows:
            if isinstance(row, dict):
                state = str(row.get("state") or "?")
                counts[state] = counts.get(state, 0) + 1
        goal = one_line(plan.get("goal") or "no goal recorded", 120)
    except (OSError, ValueError, AttributeError, TypeError):
        return "- plan: unavailable (tasks.json unreadable); do not read this as no plan"
    summary = " ".join("%s=%d" % item for item in sorted(counts.items()))
    return "- plan: %s [%s]" % (goal, summary or "no tasks")


def failure_line(repo):
    if not os.path.isfile(os.path.join(repo, ".oms", "failures.jsonl")):
        return "- last failing check: not recorded"
    try:
        done = subprocess.run(["bash", os.path.join(SCRIPTS, "fail-ledger.sh"), "--repo", repo, "list",
                               "--unresolved", "--json"], capture_output=True, text=True, timeout=10,
                              check=False, stdin=subprocess.DEVNULL)
        rows = json.loads(done.stdout).get("failures", []) if done.returncode == 0 else None
    except (OSError, ValueError, AttributeError, subprocess.SubprocessError):
        rows = None
    if rows is None:
        return "- last failing check: unavailable (fail-ledger did not answer); do not read this as none"
    rows = [r for r in rows if isinstance(r, dict) and r.get("actionable") is True]
    if not rows:
        return "- last failing check: none unresolved"
    newest = max(rows, key=lambda r: str(r.get("ts") or ""))
    text = one_line(newest.get("summary") or newest.get("cmd") or "no summary", 160)
    return "- last failing check: %s (fp: %s; oms fail-ledger show --fingerprint %s)" % (
        text, newest.get("fingerprint", "unknown"), newest.get("fingerprint", "unknown"))


def changed_line(repo):
    try:
        done = subprocess.run(["git", "-C", repo, "status", "--porcelain", "-z", "--untracked-files=normal"],
                              capture_output=True, timeout=10, check=False, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return "- changed files: unavailable (git status failed)"
    if done.returncode != 0:
        return "- changed files: unavailable (git status failed)"
    names = set()
    entries = done.stdout.decode("utf-8", "replace").split("\0")
    skip = False
    for entry in entries:
        if skip:  # the origin half of a rename record
            skip = False
            continue
        if len(entry) < 4:
            continue
        if entry[0] in "RC":
            skip = True
        name = entry[3:]
        if not name.startswith(".oms/"):
            names.add(one_line(name, 100))
    ordered = sorted(names)
    if not ordered:
        return "- changed files: none (clean tree)"
    shown = ordered[:FILES]
    more = len(ordered) - len(shown)
    return "- changed files (%d, git status): %s%s" % (len(ordered), ", ".join(shown),
                                                       " (+%d more)" % more if more else "")


def decision_lines(repo, active):
    if not active:
        return ["- decisions: not recorded (no active task)"]
    body = packet_sections(repo).get("Decisions", "")
    # `agent-task update --decision` rows render once, capped, in the Decisions block.
    stamped = sum(1 for line in body.splitlines() if STAMPED.match(line.strip()))
    plain = bullets("\n".join(line for line in body.splitlines() if not STAMPED.match(line.strip())))
    if not stamped and not plain:
        return ["- decisions: not recorded"]
    lines = []
    if stamped:
        lines.append("- decisions: %d recorded (newest under ## Decisions)" % stamped)
    if plain:
        lines += ["- decisions (task packet, newest last):"] + ["  - " + one_line(row, 160) for row in plain[-DECISIONS:]]
    return lines


def main(argv):
    if len(argv) < 1:
        return 2
    repo = os.path.realpath(argv[0])
    active = len(argv) > 1 and argv[1] == "active"
    lines = ["", "## Recorded state (mechanical; from .oms records and git, no model)", "",
             plan_line(repo), *decision_lines(repo, active), changed_line(repo), failure_line(repo)]
    text = "\n".join(lines) + "\n"
    raw = text.encode("utf-8")
    if len(raw) > CAP:
        tail = "\ntruncated: block held to %d bytes; open the sources\n" % CAP
        text = raw[:CAP - len(tail)].decode("utf-8", "ignore").rstrip("\n") + tail
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
