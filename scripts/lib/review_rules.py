#!/usr/bin/env python3
"""The repository's own review rules, and what repeated dismissals suggest.

`rules` prints the `- Review rules:` item of PROJECT.md's Verification section
as it stands in a named commit (the review's base, default HEAD), never the
working copy: a change under review cannot rewrite the rules it is reviewed
by. Exit 3 means the base declares none. When the working copy's rules differ
from the base's, one line on stderr says so.

`suggest` reads the finding events and lists dismissal reasons the owner gave
three or more times. It suggests; the owner edits PROJECT.md by hand, and
nothing here writes a rule.
"""

import argparse
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import review_findings as base  # noqa: E402

MAX_RULES_BYTES = 2000
MAX_FILE = 1024 * 1024
REPEAT = 3
MIN_SAMPLES = 5
DEMOTE_RATE = 0.7
SECTION = re.compile(r"(?ms)^## Verification[ \t]*\n(.*?)(?=^## |\Z)")
ITEM = re.compile(r"(?m)^- Review rules:[ \t]*([^\r\n]*)((?:\r?\n[ \t]+[^\r\n]*)*)")


def parse(text):
    """The declared rules as clean lines, [] when there are none or more than one declaration."""
    section = SECTION.search(text or "")
    if not section:
        return []
    found = ITEM.findall(section.group(1))
    if len(found) != 1:
        return []
    head, rest = found[0]
    lines = [base.clean(head, 400)] + [base.clean(line, 400) for line in rest.splitlines()]
    lines = [line for line in lines if line and line.lower() not in ("none", "n/a", "-")]
    kept, used = [], 0
    for line in lines:
        used += len(line.encode("utf-8")) + 1
        if used > MAX_RULES_BYTES:
            kept.append("(more rules omitted: over %d bytes)" % MAX_RULES_BYTES)
            break
        kept.append(line)
    return kept


def at_commit(repo, ref):
    try:
        done = subprocess.run(["git", "-C", repo, "show", "%s:PROJECT.md" % ref], stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0 or len(done.stdout) > MAX_FILE:
        return None
    return done.stdout.decode("utf-8", "replace")


def working(repo):
    path = os.path.join(repo, "PROJECT.md")
    if os.path.islink(path) or not os.path.isfile(path) or os.path.getsize(path) > MAX_FILE:
        return None
    return base.read_text(path, MAX_FILE)


def rules(args):
    if not re.match(r"^[A-Za-z0-9._/@^~-]{1,200}$", args.base) or args.base.startswith("-"):
        raise SystemExit("review rules: unsafe base ref")
    declared = parse(at_commit(args.repo, args.base))
    if parse(working(args.repo)) != declared:
        print("review-rules: this change edits PROJECT.md's Review rules; the copy at %s is the one in force"
              % base.clean(args.base, 80), file=sys.stderr)
    if not declared:
        return 3
    print("\n".join(declared))
    return 0


def suggest(args):
    reasons = {}
    for row in base.read_rows(os.path.join(args.artifact_dir, base.EVENTS_FILE)):
        if row.get("kind") != "review-finding-event" or row.get("state") != "dismissed":
            continue
        reason = base.clean(row.get("reason") or "", 200)
        if reason and isinstance(row.get("id"), str):
            entry = reasons.setdefault(base.normalized(reason), {"text": reason, "ids": set()})
            entry["ids"].add(row["id"])
    repeated = sorted(((len(entry["ids"]), entry["text"]) for entry in reasons.values() if len(entry["ids"]) >= REPEAT),
                      key=lambda item: (-item[0], item[1]))
    if not repeated:
        print("review rules: no dismissal reason was given for %d or more findings" % REPEAT)
        return 0
    print("review rules: dismissal reasons given for %d or more findings (a suggestion; add a rule to PROJECT.md "
          "'- Review rules:' by hand if it is one):" % REPEAT)
    for count, text in repeated[:20]:
        print("  %d finding(s): %s" % (count, text))
    return 0


def signals(args):
    """Per rule label: what became of the findings that named it. Read-only; nothing is edited or disabled."""
    findings, events = base.load(args.artifact_dir)
    table, unlabelled = {}, 0
    for ident, entry in findings.items():
        label = entry.get("rule")
        if not isinstance(label, str) or not label:
            unlabelled += 1
            continue
        state, _ = base.status(entry, events.get(ident, []))
        row = table.setdefault(label, {"rule": label, "raised": 0, "fixed": 0, "dismissed": 0, "open": 0})
        row["raised"] += 1
        row["fixed" if state in ("fixed", "verified") else state] += 1
    rows = []
    for row in table.values():
        decided = row["fixed"] + row["dismissed"]
        row["dismiss_rate"] = round(row["dismissed"] / decided, 2) if decided else None
        row["suggestion"] = ("demote?" if decided >= args.min_samples and row["dismissed"] / decided > DEMOTE_RATE
                             else "keep")
        rows.append(row)
    rows.sort(key=lambda row: (-row["raised"], row["rule"]))
    if args.json:
        print(json.dumps({"schema": base.SCHEMA, "min_samples": args.min_samples, "demote_rate": DEMOTE_RATE,
                          "unlabelled": unlabelled, "rules": rows}, ensure_ascii=False, sort_keys=True))
        return 0
    if not rows:
        print("review rules: no finding names a rule (%d unlabelled); seats add an optional \"rule\" label" % unlabelled)
        return 0
    print("%-24s %6s %6s %9s %5s %8s  suggestion" % ("rule", "raised", "fixed", "dismissed", "open", "dismiss"))
    for row in rows:
        rate = "-" if row["dismiss_rate"] is None else "%d%%" % round(row["dismiss_rate"] * 100)
        print("%-24s %6d %6d %9d %5d %8s  %s" % (row["rule"][:24], row["raised"], row["fixed"], row["dismissed"],
                                                  row["open"], rate, row["suggestion"]))
    print("%d finding(s) carry no rule label. demote? = dismissed share of decided findings above %d%% with at "
          "least %d decided; a suggestion only, no rule is edited or disabled" % (unlabelled, DEMOTE_RATE * 100,
                                                                                    args.min_samples))
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="review_rules.py")
    sub = parser.add_subparsers(dest="action", required=True)
    one = sub.add_parser("signals")
    one.add_argument("--artifact-dir", default=os.path.join(os.getcwd(), ".oms", "artifacts", "review"))
    one.add_argument("--repo", default="")
    one.add_argument("--json", action="store_true")
    one.add_argument("--min-samples", type=int, default=MIN_SAMPLES)
    one = sub.add_parser("rules")
    one.add_argument("--repo", required=True)
    one.add_argument("--base", default="HEAD")
    one = sub.add_parser("suggest")
    one.add_argument("--artifact-dir", required=True)
    args = parser.parse_args(argv)
    if args.action == "signals":
        return signals(args)
    return rules(args) if args.action == "rules" else suggest(args)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except SystemExit as stop:
        if isinstance(stop.code, str):
            print(stop.code, file=sys.stderr)
            sys.exit(2)
        raise
