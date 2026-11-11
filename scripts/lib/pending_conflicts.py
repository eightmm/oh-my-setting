#!/usr/bin/env python3
"""`oms land conflicts`: a read-only pairwise conflict preview among pending `oms/*` branches.

Each branch is merged in memory against the base and against every other
branch with `git merge-tree --write-tree`, and the result is printed as
`clean` or `conflicts in FILES`. Nothing is merged, checked out, written to a
ref or to the index, and no model is called. The preview is advisory: it is
never approval or verification evidence, and the landing order it suggests is
a labelled heuristic over the pairwise results (three branches that are
pairwise clean can still conflict together).

Every Git call goes through one hardened runner: no inherited or user
configuration, no hooks, no fsmonitor. Repository config that would run a
program (the shared execution-config check, plus merge drivers, which
`merge-tree` would start for a conflicting file) makes the preview unavailable
instead of running it. `merge-tree` writes unreferenced objects, so each call
writes into a throw-away object directory that is removed afterwards.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

LIB = os.path.dirname(os.path.abspath(__file__))
MIN_GIT = (2, 38)
DEFAULT_MAX = 12
HARD_MAX = 24
FILES_SHOWN = 5
FILES_KEPT = 200
GIT_TIMEOUT = 20
OID = re.compile(r"^[0-9a-f]{40}([0-9a-f]{24})?$")
MERGE_DRIVER_KEY = re.compile(r"^merge\..+\.driver$", re.IGNORECASE)


def stop(message, code=2):
    print("land conflicts: " + message, file=sys.stderr)
    raise SystemExit(code)


def git_env(extra=None):
    env = {key: value for key, value in os.environ.items() if not key.upper().startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_SYSTEM=os.devnull, GIT_CONFIG_GLOBAL=os.devnull,
               GIT_CONFIG_COUNT="0", GIT_TERMINAL_PROMPT="0", GIT_PAGER="cat", GIT_OPTIONAL_LOCKS="0",
               GIT_ATTR_NOSYSTEM="1", LC_ALL="C")
    env.update(extra or {})
    return env


def git(repo, *args, extra_env=None):
    """(returncode, stdout bytes); returncode None when Git could not run or timed out."""
    command = ["git", "--no-pager", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
               "-C", repo] + list(args)
    try:
        done = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, env=git_env(extra_env),
                              timeout=GIT_TIMEOUT, check=False)
    except (OSError, subprocess.SubprocessError):
        return None, b""
    return done.returncode, done.stdout


def text(data):
    return data.decode("utf-8", "replace").strip()


def git_version(repo):
    code, out = git(repo, "--version")
    found = re.search(r"(\d+)\.(\d+)", text(out)) if code == 0 else None
    return (int(found.group(1)), int(found.group(2))) if found else None


def unsafe_config(repo):
    """Reason string when repository config would run a program during a merge, else ''."""
    script = '. "$1/oms-common.sh"; oms_git_assert_safe_execution_config "$2" diff-read'
    try:
        done = subprocess.run(["bash", "-c", script, "pending-conflicts", LIB, repo], stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=30, check=False)
    except (OSError, subprocess.SubprocessError):
        return "the Git config check could not run"
    if done.returncode != 0:
        lines = (done.stderr or "").strip().splitlines()
        return lines[0][:160] if lines else "the Git config check refused this repository"
    code, out = git(repo, "config", "--no-includes", "--null", "--name-only", "--list")
    if code is None:
        return "repository Git config could not be read"
    for key in out.decode("utf-8", "replace").split("\0"):
        if MERGE_DRIVER_KEY.match(key):
            return "repository config defines a merge driver, which a merge would run"
    return ""


def resolve_base(repo, name):
    candidates = [name] if name else []
    if not name:
        code, out = git(repo, "symbolic-ref", "--short", "refs/remotes/origin/HEAD")
        if code == 0 and text(out):
            candidates.append(text(out))
        candidates += ["origin/main", "origin/master", "main", "master"]
    for candidate in candidates:
        if candidate.startswith("-"):
            stop("a base may not start with '-'")
        code, out = git(repo, "rev-parse", "--verify", "--quiet", candidate + "^{commit}")
        if code == 0 and OID.match(text(out)):
            return candidate, text(out)
    stop("no base found; pass --base REV" if not name else "--base does not name a commit")


def list_branches(repo, names):
    """[(name, sha, committer time)] for the named local branches, else every local oms/* branch."""
    if names:
        found = []
        for name in names:
            if name.startswith("-"):
                stop("a branch may not start with '-'")
            code, out = git(repo, "for-each-ref", "--format=%(refname:short)%00%(objectname)%00%(committerdate:unix)",
                            "refs/heads/" + name)
            rows = [line.split("\0") for line in text(out).splitlines()] if code == 0 else []
            exact = [row for row in rows if row[0] == name]
            if not exact:
                stop("no local branch named %s" % name)
            found.append((name, exact[0][1], int(exact[0][2] or 0)))
        return found
    code, out = git(repo, "for-each-ref", "--format=%(refname:short)%00%(objectname)%00%(committerdate:unix)",
                    "refs/heads/oms/")
    if code != 0:
        stop("cannot list branches")
    rows = []
    for line in text(out).splitlines():
        name, sha, when = (line.split("\0") + ["", "", "0"])[:3]
        if name and OID.match(sha):
            rows.append((name, sha, int(when or 0)))
    return rows


def merge_preview(repo, objects_dir, left, right):
    """('clean', []) | ('conflicts', files) | ('error', reason) for one in-memory merge."""
    scratch = tempfile.mkdtemp(prefix="oms-conflicts.")
    try:
        code, out = git(repo, "merge-tree", "--write-tree", "--name-only", "--no-messages", "-z", left, right,
                        extra_env={"GIT_OBJECT_DIRECTORY": scratch,
                                   "GIT_ALTERNATE_OBJECT_DIRECTORIES": objects_dir})
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    if code is None:
        return "error", "git timed out or could not run"
    fields = out.split(b"\0")
    if not OID.match(fields[0].decode("ascii", "replace")):
        return "error", "git merge-tree failed (exit %d)" % code
    if code == 0:
        return "clean", []
    if code == 1:
        files = sorted({one_line(f.decode("utf-8", "replace")) for f in fields[1:] if f})
        return "conflicts", files
    return "error", "git merge-tree failed (exit %d)" % code


def one_line(value, limit=200):
    value = " ".join("".join(ch if ch.isprintable() else " " for ch in value).split())
    return value if len(value) <= limit else value[: limit - 1] + "…"


def describe(state, detail, full=False):
    if state == "clean":
        return "clean"
    if state == "error":
        return "error: " + detail
    shown = detail if full else detail[:FILES_SHOWN]
    extra = "" if len(shown) == len(detail) else " +%d more" % (len(detail) - len(shown))
    return "conflicts in " + ", ".join(shown) + extra


def suggest_order(branches, base_state, pair_state):
    """Heuristic: land the branches that conflict with the fewest of the others first; ones that
    already conflict with the base go last, because they need a rebase whatever the order."""
    remaining = [b["name"] for b in branches]
    when = {b["name"]: b["committed"] for b in branches}
    order = []
    while remaining:
        def key(name):
            clashes = sum(1 for other in remaining if other != name and pair_state(name, other) != "clean")
            return (base_state[name] != "clean", clashes, when[name], name)
        pick = min(remaining, key=key)
        order.append(pick)
        remaining.remove(pick)
    return order


def main(argv):
    parser = argparse.ArgumentParser(prog="land conflicts", add_help=True)
    parser.add_argument("branches", nargs="*", help="local branches to compare (default: every local oms/* branch)")
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--base", default="")
    parser.add_argument("--max", type=int, default=DEFAULT_MAX, dest="limit")
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args(argv)
    if not 2 <= args.limit <= HARD_MAX:
        stop("--max must be between 2 and %d" % HARD_MAX)
    repo = os.path.realpath(args.repo)
    code, out = git(repo, "rev-parse", "--show-toplevel")
    if code != 0:
        stop("%s is not a git repository" % repo)
    doc = {"schema": 1, "mode": "pending-conflicts", "status": "ok", "readonly": True,
           "limit": {"max": args.limit, "hard_max": HARD_MAX}}

    def finish(status, reason, line):
        doc.update(status=status, reason=reason)
        if args.as_json:
            print(json.dumps(doc, indent=2, sort_keys=True))
        else:
            print(line)
        raise SystemExit(0)

    version = git_version(repo)
    if version is None or version < MIN_GIT:
        shown = "%d.%d" % version if version else "unknown"
        finish("unavailable", "git-too-old",
               "pending conflicts: not checked, git %s is too old (git merge-tree --write-tree needs %d.%d+); "
               "nothing was merged or written" % (shown, MIN_GIT[0], MIN_GIT[1]))
    reason = unsafe_config(repo)
    if reason:
        finish("unavailable", "unsafe-config", "pending conflicts: not checked, %s; nothing was run" % reason)

    objects_code, objects_out = git(repo, "rev-parse", "--path-format=absolute", "--git-path", "objects")
    objects_dir = text(objects_out) if objects_code == 0 else ""
    if not objects_dir or not os.path.isdir(objects_dir):
        stop("cannot locate the object directory")
    base_name, base_sha = resolve_base(repo, args.base)
    doc["base"] = {"ref": base_name, "sha": base_sha}

    found = list_branches(repo, args.branches)
    skipped, pending = [], []
    for name, sha, when in found:
        code, out = git(repo, "rev-list", "--count", "%s..%s" % (base_sha, sha))
        ahead = int(text(out)) if code == 0 and text(out).isdigit() else None
        if ahead == 0:
            skipped.append({"name": name, "reason": "already in the base"})
        elif ahead is None:
            skipped.append({"name": name, "reason": "cannot compare with the base"})
        else:
            pending.append({"name": name, "sha": sha, "ahead": ahead, "committed": when})
    pending.sort(key=lambda b: (-b["committed"], b["name"]))
    considered = len(pending)
    if considered > args.limit:
        for extra in pending[args.limit:]:
            skipped.append({"name": extra["name"], "reason": "over the --max limit (oldest first dropped)"})
        pending = pending[: args.limit]
    pending.sort(key=lambda b: (b["committed"], b["name"]))
    doc["limit"].update(considered=considered, checked=len(pending), truncated=considered > len(pending))
    doc["skipped"] = skipped
    doc["branches"] = pending

    base_state, pair_rows = {}, {}
    for branch in pending:
        state, detail = merge_preview(repo, objects_dir, base_sha, branch["sha"])
        branch["vs_base"] = {"state": state, "files": detail[:FILES_KEPT] if state == "conflicts" else [],
                             "reason": detail if state == "error" else ""}
        base_state[branch["name"]] = state
        branch["_detail"] = detail
    pairs = []
    for index, left in enumerate(pending):
        for right in pending[index + 1:]:
            state, detail = merge_preview(repo, objects_dir, left["sha"], right["sha"])
            pair_rows[(left["name"], right["name"])] = state
            pair_rows[(right["name"], left["name"])] = state
            pairs.append({"a": left["name"], "b": right["name"], "state": state,
                          "files": detail[:FILES_KEPT] if state == "conflicts" else [],
                          "reason": detail if state == "error" else "", "_detail": detail})
    doc["pairs"] = pairs
    order = suggest_order(pending, base_state, lambda a, b: pair_rows[(a, b)]) if pending else []
    doc["suggested_order"] = {"label": "suggestion", "basis": "pairwise results only", "branches": order}

    if args.as_json:
        for row in pending + pairs:
            row.pop("_detail", None)
        print(json.dumps(doc, indent=2, sort_keys=True))
        return 0

    lines = ["pending conflicts: read-only preview, nothing merged, checked out or written",
             "base %s (%s)" % (base_name, base_sha[:7]),
             "limit: %d of %d pending branch(es) checked (--max N, at most %d)" % (len(pending), considered, HARD_MAX)]
    for item in skipped:
        lines.append("skipped %s: %s" % (item["name"], item["reason"]))
    if not pending:
        lines.append("no pending oms/* branches with commits beyond the base")
    else:
        lines.append("against the base:")
        for branch in pending:
            lines.append("  %s  %s" % (branch["name"], describe(branch["vs_base"]["state"], branch["_detail"])))
        if pairs:
            lines.append("pairs:")
            for pair in pairs:
                lines.append("  %s x %s  %s" % (pair["a"], pair["b"], describe(pair["state"], pair["_detail"])))
        lines.append("suggested landing order (a suggestion, not a decision; pairwise results only, a branch "
                     "that conflicts with an earlier one needs a rebase after it lands):")
        for position, name in enumerate(order, 1):
            clashes = sorted(o for o in order if o != name and pair_rows[(name, o)] != "clean")
            note = "conflicts with " + ", ".join(clashes) if clashes else "no pairwise conflicts"
            if base_state[name] != "clean":
                note += "; needs a rebase on the base first"
            lines.append("  %d. %s  (%s)" % (position, name, note))
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
