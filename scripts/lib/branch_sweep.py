#!/usr/bin/env python3
"""Read-only report of leftover local oms/* work branches.

Groups each branch as `safe to delete` (tip is an ancestor of the base),
`landed by equivalent commits` (every commit has a patch-id twin on the base),
or `still unique`. A branch checked out in a worktree, or whose worktree
carries a live harness marker, is reported as `in use` and never offered for
deletion. The report prints the delete commands for the operator and runs none.
"""
from __future__ import annotations

import argparse
import os
import re
import shlex
import subprocess
import sys
import time

PREFIX = "refs/heads/oms/"
BASE_CANDIDATES = ("refs/remotes/origin/HEAD", "refs/remotes/origin/main",
                   "refs/heads/main", "refs/heads/master")
TIMEOUT = 20
_CTRL = re.compile(r"[\x00-\x1f\x7f]")


def _env():
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_SYSTEM=os.devnull,
               GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_COUNT="0",
               GIT_TERMINAL_PROMPT="0", GIT_OPTIONAL_LOCKS="0", GIT_PAGER="cat", LC_ALL="C")
    return env


def git(repo, *args, ok=(0,)):
    """Run Git with no inherited config, hooks or fsmonitor; None on failure."""
    cmd = ["git", "--no-pager", "-c", "core.hooksPath=" + os.devnull,
           "-c", "core.fsmonitor=false", "-C", repo, *args]
    try:
        done = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True,
                              env=_env(), timeout=TIMEOUT, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode not in ok:
        return None
    return done.stdout.decode("utf-8", "replace")


def clean(text):
    return _CTRL.sub("?", text)


def pick_base(repo):
    for ref in BASE_CANDIDATES:
        if git(repo, "rev-parse", "--verify", "-q", ref + "^{commit}") is not None:
            sym = (git(repo, "symbolic-ref", "-q", ref) or "").strip()
            return sym or ref
    return None


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def worktree_refs(repo):
    """Map branch ref -> list of human-readable references."""
    refs = {}
    out = git(repo, "worktree", "list", "--porcelain") or ""
    path = branch = None
    prunable = False

    def flush():
        if path and branch and branch.startswith(PREFIX):
            note = "worktree " + clean(path)
            if prunable:
                note += " (stale registration)"
            else:
                try:
                    with open(os.path.join(path, ".oh-my-setting-tmp"), encoding="utf-8",
                              errors="replace") as handle:
                        text = handle.read(4096)
                    match = re.search(r"^pid=(\d+)$", text, re.M)
                    if match and pid_alive(int(match.group(1))):
                        note += " (live owner pid %s)" % match.group(1)
                except OSError:
                    pass
            refs.setdefault(branch, []).append(note)

    for line in out.splitlines() + [""]:
        if not line:
            flush()
            path = branch = None
            prunable = False
        elif line.startswith("worktree "):
            path = line[len("worktree "):]
        elif line.startswith("branch "):
            branch = line[len("branch "):]
        elif line.startswith("prunable"):
            prunable = True
    return refs


def classify(repo, base, ref):
    if _ancestor(repo, ref, base):
        return "safe", 0
    unique = git(repo, "rev-list", "--cherry-pick", "--right-only", "%s...%s" % (base, ref))
    if unique is None:
        return "unique", -1
    count = len(unique.split())
    # Merge commits have no patch-id, so a branch holding one stays unique: never hides work.
    return ("landed" if count == 0 else "unique"), count


def _ancestor(repo, ref, base):
    cmd = ["git", "--no-pager", "-c", "core.hooksPath=" + os.devnull,
           "-c", "core.fsmonitor=false", "-C", repo, "merge-base", "--is-ancestor", ref, base]
    try:
        return subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True, env=_env(),
                              timeout=TIMEOUT, check=False).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def age_text(seconds):
    days = max(0, seconds) // 86400
    return "%dd" % days if days else "<1d"


def build(repo, now=None):
    now = int(time.time()) if now is None else now
    fmt = "%(refname)%09%(objectname:short)%09%(committerdate:unix)%09%(upstream:short)"
    out = git(repo, "for-each-ref", "--format=" + fmt, "refs/heads/oms/")
    if out is None:
        return None, []
    rows = [line.split("\t") for line in out.splitlines() if line]
    if not rows:
        return "", []
    base = pick_base(repo)
    refs = worktree_refs(repo)
    results = []
    for ref, sha, stamp, upstream in (r + [""] * (4 - len(r)) for r in rows):
        name = ref[len("refs/heads/"):]
        age = age_text(now - int(stamp)) if stamp.isdigit() else "?"
        item = {"name": name, "sha": sha, "age": age, "upstream": upstream or "none",
                "refs": refs.get(ref, []), "group": "unique", "ahead": -1}
        if base:
            item["group"], item["ahead"] = classify(repo, base, ref)
        if item["refs"]:
            item["group"] = "in-use"
        results.append(item)
    return base, results


def render(base, results):
    lines = ["", "## oms/* work branches (read-only report; nothing is deleted)", ""]
    if not base:
        lines.append("no base branch found (origin/HEAD, origin/main, main, master): "
                     "%d oms/* branch(es) not classified" % len(results))
        return lines
    lines.append("base: %s; %d local oms/* branch(es)" % (clean(base), len(results)))
    titles = (("safe", "safe to delete (tip is an ancestor of the base)"),
              ("landed", "landed by equivalent commits (patch-id match; owner landed by committing)"),
              ("unique", "still unique (commits not on the base)"),
              ("in-use", "in use (worktree references; not offered for deletion)"))
    for key, title in titles:
        group = [r for r in results if r["group"] == key]
        if not group:
            continue
        lines += ["", "%s: %d" % (title, len(group))]
        for r in sorted(group, key=lambda r: r["name"]):
            extra = ""
            if key == "unique" and r["ahead"] >= 0:
                extra = ", %d commit(s) not on base" % r["ahead"]
            if r["refs"]:
                extra += ", " + "; ".join(r["refs"])
            lines.append("- %s  tip %s, last commit %s ago, upstream %s%s"
                         % (clean(r["name"]), r["sha"], r["age"], clean(r["upstream"]), extra))
    cmds = []
    for r in sorted(results, key=lambda r: r["name"]):
        quoted = shlex.quote(r["name"])
        if r["group"] == "safe":
            cmds.append("git branch -d %s" % quoted)
        elif r["group"] == "landed":
            cmds.append("git branch -D %s  # was %s; restore: git branch %s %s"
                        % (quoted, r["sha"], quoted, r["sha"]))
    if cmds:
        lines += ["", "delete commands (run by hand after checking; this report never runs them):"]
        lines += ["  " + c for c in cmds]
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo")
    args = parser.parse_args(argv)
    base, results = build(args.repo)
    if base is None or not results:
        return 0
    print("\n".join(render(base, results)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
