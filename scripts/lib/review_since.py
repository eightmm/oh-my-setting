#!/usr/bin/env python3
"""Incremental review support for `peer-review --since-last`.

`resolve` reads the artifact index for the most recent completed review of the
same base whose recorded head is an ancestor of the current HEAD, and prints
either `incremental<TAB>SHA<TAB>RUN` plus the prompt note, or `full<TAB>REASON`.
It never reviews less silently: every doubt falls back to a full review.
`scope` composes the additive `review_scope` object an outcome row carries.
Nothing here changes a finding's status or counts as gate evidence.
"""
import argparse
import json
import os
import subprocess
import sys

MAX_LISTED = 20


def git(repo, *args):
    env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull, GIT_CONFIG_NOSYSTEM="1")
    result = subprocess.run(["git", "-c", "core.fsmonitor=false", "-C", repo] + list(args),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)
    return result.returncode, result.stdout.decode("utf-8", "replace").strip()


def read_rows(path):
    rows = []
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict):
                    rows.append(row)
    except OSError:
        pass
    return rows


def completed(row, scope):
    review = row.get("review")
    if isinstance(review, dict) and review.get("overall") == 2:
        return False
    return row.get("exit") in ((0, 1) if scope.get("gate") else (0,))


def open_blocking(artifact_dir, run):
    here = os.path.dirname(os.path.abspath(__file__))
    if here not in sys.path:
        sys.path.insert(0, here)
    import review_findings
    findings, events = review_findings.load(artifact_dir)
    lines = []
    for ident in sorted(findings):
        entry = findings[ident]
        if entry.get("severity") != "blocking" or run not in [seen for _, seen in entry["runs"]]:
            continue
        if review_findings.status(entry, events.get(ident, []))[0] != "open":
            continue
        lines.append("- %s %s:%s %s" % (ident, review_findings.clean(entry.get("file"), 120), entry.get("line"),
                                        review_findings.clean(entry.get("summary"), 160)))
    return lines


def resolve(args):
    code, base = git(args.repo, "rev-parse", "--verify", "--quiet", args.base + "^{commit}")
    code2, head = git(args.repo, "rev-parse", "--verify", "--quiet", "HEAD")
    if code or code2:
        print("full\tthe base or HEAD cannot be resolved")
        return 0
    scoped = []
    for row in read_rows(args.index):
        scope = row.get("review_scope")
        if row.get("kind") == "review-outcome" and isinstance(scope, dict) and scope.get("head") and scope.get("base"):
            scoped.append((row, scope))
    if not scoped:
        print("full\tno earlier review recorded its head and base")
        return 0
    first_reason = ""
    for row, scope in reversed(scoped):
        reason = ""
        if not completed(row, scope):
            reason = "the most recent earlier review did not complete"
        elif scope["base"] != base:
            reason = "the base moved since the earlier review"
        elif git(args.repo, "cat-file", "-e", scope["head"] + "^{commit}")[0] or \
                git(args.repo, "merge-base", "--is-ancestor", scope["head"], head)[0]:
            reason = "the earlier reviewed head %s is not an ancestor of HEAD (rebase or force-push)" % scope["head"][:12]
        elif scope["head"] == head:
            reason = "no commits were added since the earlier review"
        if reason:
            first_reason = first_reason or reason
            continue
        run = str(scope.get("run") or "")
        print("incremental\t%s\t%s" % (scope["head"], run))
        print("Incremental review: this branch was already reviewed. The diff below covers only the commits added since %s "
              "(earlier review %s, %s, verdict %s). Earlier code was not re-sent; report on it only where the new changes interact with it. "
              "This is not a full review." % (scope["head"][:12], run or "unknown", row.get("ts", "unknown time"),
                                              "pass" if row.get("exit") == 0 else "fail"))
        lines = open_blocking(args.artifact_dir, run) if run else []
        if lines:
            print("Blocking findings from that review that are still open (their status is unchanged; say for each whether the new commits address it):")
            print("\n".join(lines[:MAX_LISTED]))
        return 0
    print("full\t" + first_reason)
    return 0


def scope_row(args):
    with open(args.scope, encoding="utf-8") as handle:
        scope = json.load(handle)
    out = {"run": args.run, "head": scope.get("head", ""), "base": scope.get("base", ""),
           "diff_sha256": scope.get("diff_sha256", ""), "gate": args.gate == "1"}
    if scope.get("incremental"):
        out["incremental"] = True
        out["since"] = scope.get("since", "")
    print(json.dumps(out, sort_keys=True, separators=(",", ":")))
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="review_since.py")
    sub = parser.add_subparsers(dest="cmd", required=True)
    one = sub.add_parser("resolve")
    one.add_argument("--repo", required=True)
    one.add_argument("--index", required=True)
    one.add_argument("--artifact-dir", required=True)
    one.add_argument("--base", required=True)
    two = sub.add_parser("scope")
    two.add_argument("--scope", required=True)
    two.add_argument("--run", required=True)
    two.add_argument("--gate", choices=("0", "1"), required=True)
    args = parser.parse_args(argv)
    return resolve(args) if args.cmd == "resolve" else scope_row(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
