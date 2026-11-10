"""A size signal for a proposed task: how much of the repository its scope covers.

A reading aid for the person approving a proposal. It counts tracked files
under the task's `allowed` paths with `git ls-files`, the directories they sit
in and how many look like tests. No model scores anything and nothing gates:
a large scope is a hint to split the task before a worker spends its wall
clock on it, not a refusal. Files a task will create are not counted, and the
count says nothing about how much of the scope the task will actually touch.
"""
import os
import subprocess

LARGE_FILES = 40
LARGE_DIRS = 8
CAP = 5000
TIMEOUT = 5


def _is_test(path):
    name = os.path.basename(path)
    parts = path.split("/")
    return ("tests" in parts or "test" in parts or name.startswith("test_")
            or name.endswith(("_test.py", "-smoke.sh", ".test.js", ".test.ts", "_test.go")))


def signal(repo, allowed):
    """{"files", "dirs", "tests", "capped", "repo_wide", "large"} or None when Git cannot answer."""
    scope = [str(item).strip() for item in allowed or [] if isinstance(item, str) and str(item).strip()]
    repo_wide = not scope or any(item.rstrip("/") in (".", "") for item in scope)
    command = ["git", "-C", repo, "ls-files", "-z", "--"] + ([] if repo_wide else scope)
    try:
        done = subprocess.run(command, capture_output=True, timeout=TIMEOUT, check=False,
                              stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    paths = [item for item in done.stdout.decode("utf-8", "replace").split("\0") if item]
    capped = len(paths) > CAP
    paths = paths[:CAP]
    dirs = {os.path.dirname(path) for path in paths}
    tests = sum(1 for path in paths if _is_test(path))
    return {"files": len(paths), "dirs": len(dirs), "tests": tests, "capped": capped,
            "repo_wide": repo_wide,
            "large": repo_wide or capped or len(paths) > LARGE_FILES or len(dirs) > LARGE_DIRS}


def line(found):
    """One listing line for a signal, "" when there is none."""
    if not found:
        return ""
    if found["repo_wide"]:
        text = "size: repository-wide scope (%s%d tracked file(s))" % ("over " if found["capped"] else "",
                                                                      found["files"])
    else:
        text = "size: %s%d tracked file(s) in %d dir(s), %d test file(s)" % (
            "over " if found["capped"] else "", found["files"], found["dirs"], found["tests"])
    if found["large"]:
        text += ("; large for one task (advisory, over %d files or %d dirs): consider narrower allowed paths or"
                 " splitting it before apply" % (LARGE_FILES, LARGE_DIRS))
    return text
