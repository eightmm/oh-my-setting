#!/usr/bin/env python3
"""`oms ci-status suggest`: what the local records say should happen next.

Read-only. It reads what earlier commands recorded (.oms/ci.jsonl, the PR
comment packet, the merge observation, the review findings) and prints, per
matched event, the evidence, a command a person may run and how long the fact
has waited. Nothing is sent to an agent, retried, merged or recorded. A fact
that is not recorded is `unknown`, never a guess. Only with --refresh does it
call `ci-status triage` (read-only GETs) to learn whether a red job is also
red on the base. The rule table is config/ci-suggestions.json; the condition
each rule tests is the function of the same id below.

Exit 0 suggestions printed, 1 nothing recorded to go on, 2 usage.
OMS_CI_SUGGEST_NOW (ISO time) replaces the clock for tests.
"""

import argparse
import datetime
import glob
import json
import os
import re
import subprocess
import sys

RED = ("failure", "timed_out", "startup_failure")
STAMP = "%Y-%m-%dT%H:%M:%SZ"


def now():
    fixed = os.environ.get("OMS_CI_SUGGEST_NOW", "")
    if fixed:
        return datetime.datetime.strptime(fixed, STAMP).replace(tzinfo=datetime.timezone.utc)
    return datetime.datetime.now(datetime.timezone.utc)


def parse_time(value):
    try:
        return datetime.datetime.strptime(str(value), STAMP).replace(tzinfo=datetime.timezone.utc)
    except ValueError:
        return None


def clean(value, limit=160):
    text = " ".join("".join(c if c.isprintable() else " " for c in str(value)).split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def rows_of(path):
    out = []
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict):
                    out.append(row)
    except OSError:
        pass
    return out


def span(seconds):
    if seconds < 90:
        return "%ds" % seconds
    if seconds < 5400:
        return "%dm" % (seconds // 60)
    return "%dh" % (seconds // 3600)


def git_tip(repo, branch):
    done = subprocess.run(["git", "-C", repo, "rev-parse", "--verify", "-q", branch + "^{commit}"],
                          capture_output=True, text=True, check=False, stdin=subprocess.DEVNULL)
    sha = done.stdout.strip()
    return sha if done.returncode == 0 and re.fullmatch(r"[0-9a-f]{40,64}", sha) else ""


def newest_comments(repo, pr):
    best = None
    for path in glob.glob(os.path.join(repo, ".oms", "artifacts", "pr-comments", "pr%s-*.json" % pr)):
        try:
            with open(path, encoding="utf-8") as handle:
                packet = json.load(handle)
        except (OSError, ValueError):
            continue
        if isinstance(packet, dict) and isinstance(packet.get("comments"), list):
            if best is None or str(packet.get("read_at")) >= str(best[1].get("read_at")):
                best = (path, packet)
    return best


def open_blocking(harness, repo):
    """(rows or None, total). None means the findings could not be read."""
    done = subprocess.run(
        [sys.executable, "-I", os.path.join(harness, "scripts", "lib", "review_findings.py"),
         "list", "--json", "--repo", repo, "--artifact-dir", os.path.join(repo, ".oms", "artifacts", "review")],
        capture_output=True, text=True, check=False, stdin=subprocess.DEVNULL, timeout=60)
    try:
        listed = json.loads(done.stdout)["findings"]
    except (ValueError, KeyError, TypeError):
        return None, 0
    listed = [r for r in listed if isinstance(r, dict)]
    return [r for r in listed if r.get("status") == "open" and r.get("severity") == "blocking"], len(listed)


def refresh_triage(harness, branch, sha):
    """('regression'|'preexisting'|'unknown', note) from the existing read-only triage."""
    try:
        done = subprocess.run(["bash", os.path.join(harness, "scripts", "ci-status.sh"), "triage", "--sha", sha, branch],
                              capture_output=True, text=True, check=False, stdin=subprocess.DEVNULL, timeout=120)
    except (OSError, subprocess.SubprocessError):
        return "unknown", "triage could not run"
    if done.returncode == 0:
        return "unknown", "GitHub now reports nothing red for it; record again"
    bases, job = [], False
    for line in done.stdout.splitlines():
        if line.startswith("- job:"):
            job = True
        elif job and line.strip().startswith("base:"):
            bases.append(line.split(":", 1)[1].strip())
            job = False
    if done.returncode != 1 or not bases:
        return "unknown", "triage could not read the run or its jobs"
    if any(b.startswith("green on base") for b in bases):
        return "regression", "triage compared %d failing job(s) with the base" % len(bases)
    if all(b.startswith("also red on base") for b in bases):
        return "preexisting", "triage compared %d failing job(s) with the base" % len(bases)
    return "unknown", "triage could not compare every failing job with the base"


def gather(args):
    repo, branch = args.repo, args.branch
    facts, ev = {}, {}
    rows = [r for r in rows_of(args.ledger) if r.get("branch") == branch]
    tip = git_tip(repo, branch)
    if not tip and rows:
        tip = str(rows[-1].get("sha") or "")
    mine = [r for r in rows if tip and r.get("sha") == tip]
    ev["tip"], ev["rows"], ev["mine"] = tip, rows, mine
    latest = mine[-1] if mine else None
    ev["latest"] = latest
    if latest is None:
        facts["ci"] = "unknown (no row for the tip)" if rows else "unknown (nothing recorded)"
    elif latest.get("status") != "completed":
        facts["ci"] = "pending (%s, recorded %s)" % (latest.get("status"), latest.get("ts"))
    else:
        facts["ci"] = "%s (recorded %s)" % (latest.get("conclusion"), latest.get("ts"))
    ev["class"], ev["class_note"] = "unknown", ""
    if latest and latest.get("status") == "completed" and latest.get("conclusion") in RED:
        if args.refresh:
            ev["class"], ev["class_note"] = refresh_triage(args.harness, branch, tip)
            facts["base comparison"] = "%s (%s)" % (ev["class"], ev["class_note"])
        else:
            facts["base comparison"] = "unknown (not recorded; pass --refresh or run ci-status triage)"
    # Red and later green on one commit.
    seen_red, ev["flaky"] = None, None
    for row in mine:
        if row.get("status") == "completed" and row.get("conclusion") in RED:
            seen_red = row
        elif row.get("status") == "completed" and row.get("conclusion") == "success" and seen_red:
            ev["flaky"] = (seen_red, row)
    prs = [r.get("pr_number") for r in rows if r.get("pr_number")]
    pr = prs[-1] if prs else None
    found = newest_comments(repo, pr) if pr else None
    ev["comments"] = found
    if not pr:
        facts["review comments"] = "unknown (no PR number in the CI record)"
        ev["current"] = 0
    elif not found:
        facts["review comments"] = "unknown (no packet for PR #%s; draft-pr comments stores one)" % pr
        ev["current"] = 0
    else:
        mine_c = [c for c in found[1]["comments"] if isinstance(c, dict)]
        ev["current"] = sum(c.get("state") == "current" for c in mine_c)
        facts["review comments"] = "%d current of %d (packet read %s)" % (ev["current"], len(mine_c), found[1].get("read_at"))
    blocking, total = open_blocking(args.harness, repo)
    ev["blocking"], ev["findings_total"] = blocking, total
    facts["blocking findings"] = ("unknown (could not read the findings)" if blocking is None
                                  else "%d open of %d recorded" % (len(blocking), total))
    merged = [r for r in rows_of(os.path.join(repo, ".oms", "artifacts", "pr-merged", "observations.jsonl"))
              if r.get("branch") == branch]
    ev["merged"] = merged[-1] if merged else None
    facts["merge"] = ("not observed" if not merged else
                      "merged, tree %s (observed %s)" % (merged[-1].get("tree"), merged[-1].get("observed_at")))
    return facts, ev


def match(rule, ev, branch):
    """None, or (evidence text, timestamp, template values) for the rule's id."""
    rid, latest, tip = rule["id"], ev["latest"], ev["tip"]
    vals = {"branch": branch, "sha": tip}
    done = latest is not None and latest.get("status") == "completed"
    red = done and latest.get("conclusion") in RED
    if rid == "ci-not-recorded" and latest is None:
        last = ev["rows"][-1] if ev["rows"] else None
        why = ("the newest row is for %s" % str(last.get("sha"))[:12]) if last else "no row for this branch"
        return ".oms/ci.jsonl: %s" % why, None, vals
    if rid == "ci-pending" and latest is not None and not done:
        return ".oms/ci.jsonl %s: %s on %s" % (latest.get("ts"), latest.get("status"), tip[:12]), latest.get("ts"), vals
    if rid in ("ci-red", "ci-red-regression", "ci-red-preexisting") and red:
        wanted = {"ci-red": "unknown", "ci-red-regression": "regression", "ci-red-preexisting": "preexisting"}[rid]
        if ev["class"] != wanted:
            return None
        jobs = [j for j in (latest.get("failed_jobs") or []) if isinstance(j, str)]
        text = ".oms/ci.jsonl %s: %s on %s; failed jobs: %s" % (
            latest.get("ts"), latest.get("conclusion"), tip[:12], ", ".join(jobs[:6]) or "not recorded")
        if ev["class_note"]:
            text += "; " + ev["class_note"]
        return text, latest.get("ts"), vals
    if rid == "ci-flaky" and ev["flaky"]:
        bad, good = ev["flaky"]
        names = [n for n in (bad.get("failed_jobs") or []) + (bad.get("failed_checks") or []) if isinstance(n, str)]
        return ".oms/ci.jsonl: red %s, green %s on %s; red names: %s" % (
            bad.get("ts"), good.get("ts"), tip[:12], ", ".join(names[:6]) or "not recorded"), None, vals
    if rid == "review-comments-current" and ev["current"]:
        path, packet = ev["comments"]
        vals["packet"] = ".oms/artifacts/pr-comments/" + os.path.basename(path)
        return "%s read %s: %d comment(s) on the PR head" % (vals["packet"], packet.get("read_at"), ev["current"]), \
            packet.get("read_at"), vals
    if rid == "blocking-findings-open" and ev["blocking"]:
        first = ev["blocking"][0]
        return "review findings: %d open blocking, e.g. %s %s" % (
            len(ev["blocking"]), first.get("id"), clean(first.get("summary"), 80)), None, vals
    if rid == "ready-to-land" and done and latest.get("conclusion") == "success":
        if ev["blocking"] is None or ev["blocking"] or ev["current"] or ev["merged"]:
            return None
        if ev["rows"] and any(r.get("pr_number") for r in ev["rows"]) and not ev["comments"]:
            return None  # comments are unknown, so "nothing stands in the way" cannot be said
        return ".oms/ci.jsonl %s: success on %s; no open blocking finding, no current comment" % (
            latest.get("ts"), tip[:12]), latest.get("ts"), vals
    if rid == "merged-tree-changed" and ev["merged"] and ev["merged"].get("tree") == "changed-after-review":
        m = ev["merged"]
        vals.update(reviewed_tree=m.get("reviewed_tree", "?"), merged_tree=m.get("merged_tree", "?"))
        return "pr-merged observation %s: tree changed-after-review (merged %s, reviewed %s)" % (
            m.get("observed_at"), str(m.get("merged_tree"))[:12], str(m.get("reviewed_tree"))[:12]), None, vals
    return None


def build(args, rules):
    facts, ev = gather(args)
    clock, out = now(), []
    for rule in rules:
        hit = match(rule, ev, args.branch)
        if not hit:
            continue
        text, stamp, vals = hit
        item = {"rule": rule["id"], "event": rule["event"], "say": rule["say"], "evidence": text,
                "action": rule["action"].format(**{k: vals.get(k, "?") for k in
                                                   ("branch", "sha", "packet", "reviewed_tree", "merged_tree")})}
        line = rule.get("escalate_after_s")
        began = parse_time(stamp) if stamp else None
        if line and began:
            waited = max(0, int((clock - began).total_seconds()))
            item.update(waiting_s=waited, escalate_after_s=line, over=waited > line, escalate_to=rule["escalate_to"])
        elif line:
            item.update(waiting_s=None, escalate_after_s=line, over=None, escalate_to=rule["escalate_to"])
        out.append(item)
    recorded = bool(ev["rows"] or ev["comments"] or ev["merged"] or ev["findings_total"])
    return facts, ev, out, recorded


def main(argv):
    parser = argparse.ArgumentParser(prog="ci-status suggest")
    for name in ("--harness", "--repo", "--ledger", "--branch"):
        parser.add_argument(name, required=True)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--refresh", action="store_true")
    try:
        args = parser.parse_args(argv)
    except SystemExit:
        return 2
    try:
        with open(os.path.join(args.harness, "config", "ci-suggestions.json"), encoding="utf-8") as handle:
            rules = json.load(handle)["rules"]
    except (OSError, ValueError, KeyError):
        print("ci-status suggest: the rule table config/ci-suggestions.json is unreadable", file=sys.stderr)
        return 2
    facts, ev, suggestions, recorded = build(args, rules)
    if args.json:
        print(json.dumps({"schema": 1, "kind": "oms-ci-suggest", "branch": args.branch, "sha": ev["tip"],
                          "refreshed": args.refresh, "recorded": recorded, "facts": facts,
                          "suggestions": suggestions}, ensure_ascii=False, sort_keys=True))
        return 0 if recorded else 1
    print("ci-status suggest [%s]%s: %d suggestion(s) from recorded facts%s" % (
        args.branch, " @ " + ev["tip"][:12] if ev["tip"] else "", len(suggestions),
        "" if recorded else "; nothing is recorded to go on"))
    for key, value in facts.items():
        print("  %s: %s" % (key, value))
    for item in suggestions:
        print("- %s: %s" % (item["rule"], item["event"]))
        print("    evidence: %s" % item["evidence"])
        print("    why: %s" % item["say"])
        print("    run: %s" % item["action"])
        if item.get("escalate_after_s"):
            line = span(item["escalate_after_s"])
            if item["waiting_s"] is None:
                print("    waiting: unknown (no usable timestamp); the line is %s" % line)
            elif item["over"]:
                print("    waiting %s, over the %s line: %s" % (span(item["waiting_s"]), line, item["escalate_to"]))
            else:
                print("    waiting %s, under the %s line" % (span(item["waiting_s"]), line))
    print("suggestions only: nothing was sent to an agent, retried, merged or recorded; a person decides")
    return 0 if recorded else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
