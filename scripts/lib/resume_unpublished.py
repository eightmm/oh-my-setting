"""Bounded unpublished-work hint for SessionStart."""

import subprocess
import sys
import time


MAX_QUERIES = 32
QUERY_SECONDS = 2.0
TOTAL_SECONDS = 4.0
MAX_OTHER_NAMES = 8


class _QueryBudgetExceeded(Exception):
    pass


class _GitQueries:
    def __init__(self, repo, max_queries, seconds):
        self.repo = str(repo)
        self.remaining = max_queries
        self.deadline = time.monotonic() + seconds

    def run(self, *args):
        remaining = self.deadline - time.monotonic()
        if self.remaining <= 0 or remaining <= 0:
            raise _QueryBudgetExceeded
        self.remaining -= 1
        try:
            done = subprocess.run(["git", "-C", self.repo, *args], stdin=subprocess.DEVNULL,
                                  capture_output=True, text=True,
                                  timeout=min(QUERY_SECONDS, remaining), check=False)
        except subprocess.TimeoutExpired:
            if time.monotonic() >= self.deadline:
                raise _QueryBudgetExceeded
            return None
        except (OSError, subprocess.SubprocessError):
            return None
        return done.stdout.rstrip("\n") if done.returncode == 0 else None


def _incomplete(branches, budget=False):
    detail = ("%d worktree branch(es) unexamined" % len(branches) if branches
              else "worktree scan incomplete")
    return "- unpublished: " + detail + (" (query budget)" if budget else "")


def summarize(repo, max_queries=MAX_QUERIES, seconds=TOTAL_SECONDS):
    """Prefer the current branch; never treat a failed or skipped query as clean."""
    git = _GitQueries(repo, max_queries, seconds).run
    branches = None
    try:
        remotes = git("remote")
        if remotes is None:
            return _incomplete(branches)
        if not remotes:
            return ""
        worktrees = git("worktree", "list", "--porcelain")
        if worktrees is None:
            return _incomplete(branches)
        prefix = "branch refs/heads/"
        branches = list(dict.fromkeys(line[len(prefix):] for line in worktrees.split("\n")
                                      if line.startswith(prefix)))
        here = git("symbolic-ref", "--quiet", "HEAD")
        here = here[len("refs/heads/"):] if here and here.startswith("refs/heads/") else None
        if here:
            branches = [here, *(name for name in branches if name != here)]
        if not branches:
            return ""
        # One ref snapshot replaces per-branch upstream and base-tree queries.
        # Use its object IDs throughout, so a ref moving mid-hint cannot mix trees.
        metadata = git("for-each-ref", "--format=%(refname)%00%(objectname)%00%(upstream)%00%(symref)%00%(tree)",
                       "refs/heads/", "refs/remotes/")
        if metadata is None:
            return _incomplete(branches)
        refs = {}
        for line in metadata.split("\n"):
            fields = line.split("\0")
            if len(fields) != 5:
                return _incomplete(branches)
            refs[fields[0]] = fields[1:]
    except _QueryBudgetExceeded:
        return _incomplete(branches, budget=True)

    default = refs.get("refs/remotes/origin/HEAD", ["", "", "", ""])[2]
    if default not in refs:
        default = next((ref for ref in ("refs/remotes/origin/main", "refs/remotes/origin/master")
                        if ref in refs), None)
    examined = {}

    def ahead(branch):
        head = refs.get("refs/heads/" + branch)
        if not head:
            return None, None, False
        base_ref = head[1] if head[1] in refs else default
        base = refs.get(base_ref)
        if not base:
            return None, None, False
        label = base_ref.removeprefix("refs/remotes/").removeprefix("refs/heads/")
        pair = (base[0], head[0])
        if pair in examined:
            return examined[pair], label, False
        if head[0] == base[0]:
            examined[pair] = 0
            return 0, label, False
        revision_range = "%s...%s" % pair
        count = git("rev-list", "--cherry-pick", "--right-only", "--count", revision_range)
        if count is None or not count.isdigit():
            return None, label, False
        count = int(count)
        # Preserve patch-equivalent and merge-only suppression. On older Git or
        # a merge conflict, count the merge conservatively rather than hide work.
        try:
            if count and git("rev-list", "--cherry-pick", "--right-only", "--no-merges",
                             "--count", revision_range) == "0":
                merged = git("merge-tree", "--write-tree", *pair)
                if merged and merged.split()[0] == base[3]:
                    count = 0
        except _QueryBudgetExceeded:
            # Optional merge suppression cannot erase an already proven count.
            return count, label, True
        examined[pair] = count
        return count, label, False

    parts, others, unexamined, budget = [], [], 0, False
    for index, branch in enumerate(branches):
        try:
            count, base, exhausted = ahead(branch)
        except _QueryBudgetExceeded:
            unexamined += len(branches) - index
            budget = True
            break
        if count is None:
            unexamined += 1
        elif count:
            if branch == here:
                parts.append("%d commit(s) on %s not on %s" % (count, branch, base))
            else:
                others.append("%s(%d)" % (branch, count))
        if exhausted:
            unexamined += len(branches) - index - 1
            budget = True
            break
    if others:
        parts.append("other worktrees: " + ", ".join(others[:MAX_OTHER_NAMES])
                     + (" +%d more" % (len(others) - MAX_OTHER_NAMES) if len(others) > MAX_OTHER_NAMES else ""))
    if unexamined:
        parts.append("%d worktree branch(es) unexamined" % unexamined + (" (query budget)" if budget else ""))
    return "- unpublished: " + "; ".join(parts) if parts else ""


if __name__ == "__main__":
    try:
        hint = summarize(sys.argv[1])
        if hint:
            print(hint)
    except (OSError, ValueError, subprocess.SubprocessError):
        pass  # A best-effort SessionStart hint must not prevent the session.
