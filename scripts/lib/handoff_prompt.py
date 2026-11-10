#!/usr/bin/env python3
"""A next-session prompt drafted from what OMS already recorded.

`oms session-handoff prompt` prints Markdown a person can read, edit and paste
into a fresh session of any provider. Nothing here calls a model, writes OMS
state, runs a verifier or approves anything: it asks the existing readers (the
active task packet, `runtime evidence matrix`, `runtime next --explain`,
`journal show --decisions`, `peer-review findings list`) and lays their answers
into fixed sections. Every line names the id of the record it came from; a
source with nothing recorded says "not recorded" and a reader that failed says
"unavailable", so absence is never silently dropped or read as success.

Every line passes the shared secret scan before it is printed (a flagged line
is replaced by a notice that keeps its source id), and the whole output is held
under --max-bytes with an explicit `truncated:` line.
"""

import argparse
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import time

LIB = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.dirname(LIB)
MIN_BYTES, MAX_BYTES, DEFAULT_BYTES = 512, 65536, 4096
READ_TIMEOUT = 40
PACKET_LIMIT = 64 * 1024
CAPS = {"State": 10, "Decisions": 6, "Open findings": 8, "Verified / Not verified": 12}
# Sections whose lines are dropped last when the byte bound bites.
KEEP = ("Goal", "Next step", "Pointers")
SECTIONS = ("Goal", "State", "Decisions", "Open findings", "Verified / Not verified", "Next step", "Pointers")
SCAN = ('. "$1/agent-memory-common.sh" || exit 2; shift; for f in "$@"; do rc=0; '
        'agent_memory_file_secret_status "$f" 2>/dev/null || rc=$?; printf "%s\\n" "$rc"; done')


def one_line(value, limit=200):
    text = "".join(" " if ch in "\t\r\n" else ch for ch in str(value)
                   if ch in "\t\r\n" or (ord(ch) >= 32 and not 127 <= ord(ch) <= 159))
    text = " ".join(text.split())
    home = os.environ.get("HOME", "")
    if home and home != "/":
        text = text.replace(home, "~")
    text = re.sub(r"/hom[e]/[A-Za-z0-9._-]+|/User[s]/[A-Za-z0-9._-]+", "~", text)
    return text if len(text) <= limit else text[:limit - 3] + "..."


def read(argv, repo):
    """The reader's stdout, or None when it could not answer (never an empty guess)."""
    try:
        done = subprocess.run(argv, cwd=repo, capture_output=True, text=True, timeout=READ_TIMEOUT,
                              check=False, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout if done.returncode == 0 else None


def read_json(argv, repo):
    out = read(argv, repo)
    if out is None:
        return None
    try:
        value = json.loads(out)
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def packet_sections(repo):
    """Section name -> text of the active task packet, or {} (no symlinks, bounded)."""
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
    return {name: body.strip() for name, body in re.findall(r"^## ([^\n]+)\n(.*?)(?=^## |\Z)", text, re.M | re.S)}


def bullets(body):
    return [line[2:].strip() for line in body.splitlines() if line.startswith("- ") and line[2:].strip()]


def unavailable(reader):
    return "unavailable: %s did not answer; do not read this as nothing recorded" % reader


def build(repo):
    """{section: [(text, source)]} plus the ids the pointers need."""
    out = {name: [] for name in SECTIONS}
    task = read_json(["bash", os.path.join(SCRIPTS, "agent-task.sh"), "--repo", repo, "status", "--json"], repo)
    matrix = read_json(["bash", os.path.join(SCRIPTS, "runtime.sh"), "--repo", repo, "evidence", "matrix",
                        "--limit", "100"], repo)
    inbox = read_json(["bash", os.path.join(SCRIPTS, "runtime.sh"), "--repo", repo, "next", "--explain",
                       "--limit", "100"], repo)
    plan_text = read(["bash", os.path.join(SCRIPTS, "agent-plan.sh"), "--repo", repo, "status"], repo)
    decisions = read_json(["bash", os.path.join(SCRIPTS, "journal.sh"), "--repo", repo, "show", "--decisions",
                           "--json"], repo)
    findings = read_json(["bash", os.path.join(SCRIPTS, "peer-review.sh"), "findings", "list", "--json",
                          "--repo", repo, "--artifact-dir", os.path.join(repo, ".oms", "artifacts", "review")], repo)
    sections = packet_sections(repo)

    active = isinstance(task, dict) and task.get("present") is True and task.get("status") != "closed"
    task_id = str(task.get("task_id")) if active else ""
    task_src = "task " + task_id if active else "agent-task status"
    plan_id = str(inbox.get("plan_id") or "") if inbox else ""

    # Goal
    goal = out["Goal"]
    if task is None:
        goal.append((unavailable("agent-task status"), "agent-task status"))
    elif active and sections.get("Goal"):
        goal.append(("task goal: " + one_line(sections["Goal"], 300), task_src))
    else:
        goal.append(("task goal: not recorded (no active task)", task_src))
    plan_goal = ""
    for line in (plan_text or "").splitlines():
        if line.startswith("goal: "):
            plan_goal = line[6:]
    if plan_text is None:
        goal.append((unavailable("agent-plan status"), "agent-plan status"))
    elif plan_goal:
        goal.append(("plan goal: " + one_line(plan_goal, 300), "plan " + (plan_id or "agent-plan status")))
    else:
        goal.append(("plan goal: not recorded", "agent-plan status"))
    criteria = ((matrix or {}).get("criteria") or {}).get("items") or []
    if matrix is None:
        goal.append((unavailable("runtime evidence matrix"), "runtime evidence matrix"))
    elif criteria:
        ids = [str(row.get("criterion_id")) for row in criteria[:12]]
        more = len(criteria) - len(ids)
        goal.append(("criteria ids: " + ", ".join(ids) + (" (+%d more)" % more if more > 0 else ""),
                     "runtime evidence matrix"))
    else:
        goal.append(("criteria ids: not recorded", "runtime evidence matrix"))

    # State
    state = out["State"]
    tasks = ((inbox or {}).get("tasks") or {}).get("items") or []
    if inbox is None:
        state.append((unavailable("runtime next --explain"), "runtime next --explain"))
    elif not tasks:
        state.append(("plan tasks: not recorded", "runtime next --explain"))
    else:
        counts = {}
        for row in tasks:
            counts[row.get("state")] = counts.get(row.get("state"), 0) + 1
        summary = ", ".join("%s %d" % (name, counts[name]) for name in sorted(counts, key=str))
        state.append(("plan tasks: %d (%s)" % (len(tasks), summary), "plan " + (plan_id or "runtime next --explain")))
        done = [str(row.get("task_id")) for row in tasks if row.get("state") == "done"]
        if done:
            state.append(("done: " + ", ".join(done[:12]) + (" (+%d more)" % (len(done) - 12) if len(done) > 12 else ""),
                          "plan " + (plan_id or "runtime next --explain")))
        for row in tasks:
            if row.get("state") == "done":
                continue
            waits = [str(w.get("task_id")) + "[" + str(w.get("state")) + "]"
                     for w in ((row.get("depends_waiting") or {}).get("items") or [])]
            reasons = [str(r.get("code")) for r in row.get("reasons") or []]
            kind = "ready" if row.get("state") == "ready" and not waits and row.get("canonical_actionable") else \
                ("blocked" if row.get("state") == "blocked" or waits else str(row.get("state")))
            text = "%s %s: %s" % (kind, row.get("task_id"), one_line(row.get("title"), 80))
            if reasons:
                text += " -- " + ", ".join(reasons)
            if waits:
                text += "; waits on " + ", ".join(waits)
            state.append((text, "task %s" % row.get("task_id")))

    # Decisions
    rows = (decisions or {}).get("decisions")
    if decisions is None or not isinstance(rows, list):
        out["Decisions"].append((unavailable("journal show --decisions"), "journal show --decisions"))
    elif not rows:
        out["Decisions"].append(("not recorded", "journal show --decisions"))
    else:
        for row in sorted((r for r in rows if isinstance(r, dict)),
                          key=lambda r: str(r.get("occurred_at") or ""), reverse=True):
            out["Decisions"].append(("%s %s" % (str(row.get("occurred_at") or "")[:10], one_line(row.get("text"), 160)),
                                     str(row.get("event_id") or "journal show --decisions")))

    # Open findings
    rows = (findings or {}).get("findings")
    if findings is None or not isinstance(rows, list):
        out["Open findings"].append((unavailable("peer-review findings list"), "peer-review findings list"))
    elif not rows:
        out["Open findings"].append(("not recorded", "peer-review findings list"))
    else:
        opened = [r for r in rows if isinstance(r, dict) and r.get("status") == "open"]
        if not opened:
            out["Open findings"].append(("none open among %d recorded" % len(rows), "peer-review findings list"))
        for row in sorted(opened, key=lambda r: (r.get("severity") != "blocking", str(r.get("id")))):
            out["Open findings"].append(("%s %s:%s -- %s" % (row.get("severity"), one_line(row.get("file"), 80),
                                                              row.get("line"), one_line(row.get("summary"), 140)),
                                         str(row.get("id"))))

    # Verified / Not verified: only what the records say; nothing was re-run for this prompt.
    ver = out["Verified / Not verified"]
    ver.append(("recorded results only; nothing was re-run to write this prompt", "this command"))
    if active:
        ver.append(("task gate: %s (as recorded; the gate does not check this prompt)" % task.get("verification", "unknown"),
                    task_src))
        for entry in reversed(bullets(sections.get("Verification", ""))[-3:]):
            stamp = entry.split(" ", 1)[0]
            ver.append(("recorded: " + one_line(entry, 220), "task %s verification %s" % (task_id, stamp)))
        if not bullets(sections.get("Verification", "")):
            ver.append(("task verification entries: not recorded", task_src))
    else:
        ver.append(("task verification: not recorded (no active task)", task_src))
    if matrix is not None and criteria:
        good = [r for r in criteria if r.get("status") == "verified"]
        rest = [r for r in criteria if r.get("status") != "verified"]
        for row in good[:4]:
            ver.append(("verified %s (%s)" % (row.get("criterion_id"), one_line(row.get("criterion"), 100)),
                        "runtime evidence matrix"))
        for row in rest[:6]:
            ver.append(("NOT verified %s [%s]: %s" % (row.get("criterion_id"), row.get("status"),
                                                    one_line(row.get("criterion"), 100)), "runtime evidence matrix"))
    elif matrix is None:
        ver.append((unavailable("runtime evidence matrix"), "runtime evidence matrix"))
    else:
        ver.append(("criteria evidence: not recorded", "runtime evidence matrix"))

    # Next step
    if task is None:
        out["Next step"].append((unavailable("agent-task status"), "agent-task status"))
    elif active and sections.get("Next Step"):
        out["Next step"].append((one_line(sections["Next Step"], 300), task_src))
    else:
        out["Next step"].append(("not recorded", task_src))

    # Pointers
    pointers = out["Pointers"]
    pointers.append(("task packet: `oms agent-task show`", task_src))
    pointers.append(("plan: `oms agent-plan status`; reasons: `oms runtime next --explain --format text`",
                     "plan " + (plan_id or "agent-plan status")))
    pointers.append(("criteria evidence: `oms runtime evidence matrix --format text`", "runtime evidence matrix"))
    pointers.append(("decisions: `oms journal show --decisions`", "journal show --decisions"))
    pointers.append(("findings: `oms peer-review findings list --status open`", "peer-review findings list"))
    names = []
    try:
        directory = os.path.join(repo, ".oms", "handoffs")
        names = sorted((n for n in os.listdir(directory) if n.endswith(".md")),
                       key=lambda n: os.path.getmtime(os.path.join(directory, n)), reverse=True)
    except OSError:
        names = []
    if names:
        pointers.append(("previous handoff: `oms session-handoff show %s`" % one_line(names[0], 120), names[0]))
    else:
        pointers.append(("previous handoff: not recorded", "oms session-handoff list"))
    return out


def scan(texts):
    """Per text True (flagged), False (clean); None for all when the scan cannot run (fail closed)."""
    if not texts:
        return []
    try:
        with tempfile.TemporaryDirectory(prefix="oms-handoff-prompt.") as scratch:
            paths = []
            for index, text in enumerate(texts):
                path = os.path.join(scratch, str(index))
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(text + "\n")
                paths.append(path)
            done = subprocess.run(["bash", "-c", SCAN, "scan", LIB, *paths], capture_output=True, text=True,
                                  timeout=120, check=False, stdin=subprocess.DEVNULL)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    verdicts = done.stdout.split()
    if done.returncode != 0 or len(verdicts) != len(texts) or any(v not in ("0", "1") for v in verdicts):
        return None
    return [v == "0" for v in verdicts]


def render(repo, max_bytes):
    data = build(repo)
    omitted = {}
    for name, cap in CAPS.items():
        if len(data[name]) > cap:
            omitted[name] = len(data[name]) - cap
            del data[name][cap:]
    flat = [(name, index) for name in SECTIONS for index in range(len(data[name]))]
    verdicts = scan([data[name][index][0] + " " + data[name][index][1] for name, index in flat])
    withheld = 0
    for position, (name, index) in enumerate(flat):
        if verdicts is None or verdicts[position]:
            withheld += 1
            data[name][index] = ("withheld: this line matched the sensitive-content scan; open the source",
                                 data[name][index][1])
    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    head = read(["git", "-C", repo, "rev-parse", "--short", "HEAD"], repo)
    header = ["# Next-session prompt (draft: review and edit before use)", "",
              "Drafted mechanically from OMS records at %s; no model was called. Recorded text is a claim, not "
              "fresh verification: recheck the source before acting." % stamp,
              "", "repo head: %s [src: git]" % (head.strip() if head else "not recorded")]
    if verdicts is None:
        header.append("scan: the sensitive-content scan could not run, so every line below is withheld")

    def compose():
        lines = list(header)
        for name in SECTIONS:
            lines += ["", "## " + name]
            lines += ["- %s [src: %s]" % item for item in data[name]]
            if omitted.get(name):
                lines.append("- (%d more not shown) [src: %s]" % (omitted[name], name))
        total = sum(omitted.values())
        notes = "; ".join("%s %d omitted" % (n, c) for n, c in omitted.items() if c)
        lines += ["", "truncated: " + ("%d line(s) omitted (%s); raise --max-bytes or open the sources" % (total, notes)
                                       if total else "no")]
        if withheld:
            lines.append("withheld: %d line(s) matched the sensitive-content scan" % withheld)
        return "\n".join(lines) + "\n"

    text = compose()
    while len(text.encode("utf-8")) > max_bytes:
        # Drop from the section with the most lines; the goal, next step and pointers go last.
        pool = [n for n in SECTIONS if n not in KEEP and data[n]] or [n for n in SECTIONS if len(data[n]) > 1]
        if not pool:
            break
        victim = max(pool, key=lambda n: len(data[n]))
        data[victim].pop()
        omitted[victim] = omitted.get(victim, 0) + 1
        text = compose()
    raw = text.encode("utf-8")
    if len(raw) > max_bytes:
        tail = "\ntruncated: hard cut at --max-bytes %d\n" % max_bytes
        room = max_bytes - len(tail.encode("utf-8"))
        text = raw[:max(room, 0)].decode("utf-8", "ignore") + tail
    return text


def write_new(path, text):
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except FileExistsError:
        raise SystemExit("session-handoff: refusing to overwrite %s" % one_line(path, 120))
    except OSError as error:
        raise SystemExit("session-handoff: cannot create %s: %s" % (one_line(path, 120), error.strerror))
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(text)
    if not stat.S_ISREG(os.stat(path).st_mode):
        raise SystemExit("session-handoff: output is not a regular file")


def main(argv):
    parser = argparse.ArgumentParser(prog="session-handoff.sh prompt")
    parser.add_argument("--repo", required=True)
    parser.add_argument("--max-bytes", type=int, default=DEFAULT_BYTES)
    parser.add_argument("--output")
    args = parser.parse_args(argv)
    if not MIN_BYTES <= args.max_bytes <= MAX_BYTES:
        raise SystemExit("session-handoff: --max-bytes must be %d-%d" % (MIN_BYTES, MAX_BYTES))
    text = render(os.path.realpath(args.repo), args.max_bytes)
    if args.output:
        write_new(args.output, text)
        print(args.output)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except BrokenPipeError:
        sys.exit(0)
    except SystemExit as stop:
        if isinstance(stop.code, str):
            print(stop.code, file=sys.stderr)
            sys.exit(2)
        raise
