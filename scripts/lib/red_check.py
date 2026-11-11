#!/usr/bin/env python3
"""Advisory: do the tests a patch adds or changes fail on the base commit?

A test that also passes without the change tests nothing about the change.
For each test file in the patch this applies only that file's hunks to a
scratch worktree at the base, runs the operator's command there and reports
one of `fails on base (good)`, `passes on base (does not test the change)` or
`could not run`. The command first runs once on the bare base: a base that is
already red, or a command that cannot run, makes every file `could not run`
instead of a misleading `good`.

Operator-invoked only (peer-review advise --red-check, patch-admit --red-check).
It is bounded by one total time budget, always removes the scratch worktree,
never touches the working tree, never retries, and its result is an advisory
note, not admission evidence: it changes no verdict. No model is called.
Caveat: each file is applied alone, so a test file that needs a helper added
by the same patch fails on base for that reason, not because it tests the change.
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
SCRATCH = os.path.join(ROOT, "scripts", "scratch-worktree.sh")
# The test-file selector patch-admit's "patch weakens the tests" gate uses.
TEST_PATH = re.compile(
    r"(^|/)(tests?|spec)/|(^|/)test_[^/]+$|[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$")
FAILS, PASSES, NOT_RUN = "fails on base (good)", "passes on base (does not test the change)", "could not run"
DEFAULT_TIMEOUT, DEFAULT_MAX_FILES = 300, 8


def _env():
    env = {key: value for key, value in os.environ.items()
           if key not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE")}
    env.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
    return env


def split_patch(patch):
    """[(path or None, status, chunk bytes)] per file of a git diff; the path is
    None when the header cannot be read without guessing (quoted names)."""
    files, current = [], None
    for line in patch.splitlines(keepends=True):
        if line.startswith(b"diff --git "):
            current = [line, [line]]
            files.append(current)
        elif current is not None:
            current[1].append(line)
    rows = []
    for header, lines in files:
        chunk = b"".join(lines)
        rest = header.rstrip(b"\r\n")[len(b"diff --git "):]
        path = None
        if rest.startswith(b"a/") and b'"' not in rest:
            half = (len(rest) - 5) // 2
            if half > 0 and rest == b"a/" + rest[2:2 + half] + b" b/" + rest[2:2 + half]:
                path = rest[2:2 + half]
            else:
                for line in lines:
                    if line.startswith(b"rename to ") and b'"' not in line:
                        path = line[len(b"rename to "):].rstrip(b"\r\n")
        status = "modified"
        for line in lines:
            if line.startswith(b"new file mode"):
                status = "added"
            elif line.startswith(b"deleted file mode"):
                status = "deleted"
            elif line.startswith(b"@@") or line.startswith(b"GIT binary patch"):
                break
        rows.append((path.decode("utf-8", "replace") if path else None, status, chunk))
    return rows


def _run_command(verify, cwd, timeout, name):
    """(exit code or None, note). Process group killed on timeout; output is
    discarded, only the exit code is read."""
    env = _env()
    env["OMS_RED_CHECK"] = "1"
    env["OMS_RED_CHECK_FILE"] = name
    command = verify.replace("{file}", name) if "{file}" in verify else verify
    with tempfile.TemporaryFile() as sink:
        proc = subprocess.Popen(["bash", "-c", command], cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                stdout=sink, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = proc.wait(timeout=max(1, timeout))
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except OSError:
                pass
            proc.wait()
            return None, "timed out"
        finally:
            try:
                os.killpg(proc.pid, signal.SIGKILL)  # stragglers of a finished command
            except OSError:
                pass
    if code < 0:
        return None, "killed by signal %d" % -code
    if code in (126, 127):
        return None, "command could not start (exit %d)" % code
    return code, ""


def _git(worktree, *args, data=None):
    return subprocess.run(["git", "-c", "core.fsmonitor=false", "-C", worktree, *args], input=data,
                          capture_output=True, env=_env(), check=False)


def _reset(worktree):
    _git(worktree, "reset", "--hard", "-q", "HEAD")
    _git(worktree, "clean", "-fdxq")


def check(repo, patch, verify, base="HEAD", timeout=DEFAULT_TIMEOUT, max_files=DEFAULT_MAX_FILES):
    """Result dict; never raises for an environment problem."""
    result = {"schema": 1, "advisory": True, "base": base, "files": [], "cleaned": True}
    tests = [row for row in split_patch(patch) if row[0] is None or TEST_PATH.search(row[0])]
    if not tests:
        result["note"] = "no test file in the patch"
        return result
    deadline = time.monotonic() + max(1, timeout)

    def rows(reason, only=None):
        for path, status, _ in tests:
            if only is None or path in only:
                result["files"].append({"path": path or "(unreadable path)", "status": status,
                                        "result": NOT_RUN, "detail": reason})

    scratch = ""
    old_handlers = {}

    def _stop(number, _frame):
        raise SystemExit(128 + number)

    try:
        for number in (signal.SIGTERM, signal.SIGHUP):
            old_handlers[number] = signal.signal(number, _stop)
        added = subprocess.run(["bash", SCRATCH, "add", "--repo", repo, "--base", base,
                                "--owner-pid", str(os.getpid())], capture_output=True, text=True,
                               env=_env(), check=False)
        scratch = added.stdout.strip().splitlines()[-1] if added.returncode == 0 and added.stdout.strip() else ""
        if not scratch:
            detail = (added.stderr.strip().splitlines() or ["scratch worktree unavailable"])[-1]
            rows(detail[:160])
            return result
        control, note = _run_command(verify, scratch, deadline - time.monotonic(), "")
        result["control_exit"] = control
        if control is None or control != 0:
            rows("the command %s on the bare base" % (note or "already fails (exit %d)" % control))
            return result
        for index, (path, status, chunk) in enumerate(tests):
            entry = {"path": path or "(unreadable path)", "status": status}
            result["files"].append(entry)
            if path is None:
                entry.update(result=NOT_RUN, detail="path cannot be read from the patch header")
            elif status == "deleted":
                entry.update(result=NOT_RUN, detail="the patch deletes this test file")
            elif index >= max_files:
                entry.update(result=NOT_RUN, detail="over the %d-file cap" % max_files)
            elif deadline - time.monotonic() < 1:
                entry.update(result=NOT_RUN, detail="time budget spent")
            elif _git(scratch, "apply", "--whitespace=nowarn", "-", data=chunk).returncode != 0:
                entry.update(result=NOT_RUN, detail="the test change does not apply on the base")
            else:
                code, note = _run_command(verify, scratch, deadline - time.monotonic(), path)
                if code is None:
                    entry.update(result=NOT_RUN, detail=note)
                elif code == 0:
                    entry.update(result=PASSES, detail="exit 0")
                else:
                    entry.update(result=FAILS, detail="exit %d" % code)
                _reset(scratch)
        return result
    finally:
        if scratch:
            removed = subprocess.run(["bash", SCRATCH, "remove", "--repo", repo, "--force", scratch],
                                     capture_output=True, env=_env(), check=False)
            if removed.returncode != 0 or os.path.exists(scratch):
                result["cleaned"] = False
                result["leftover"] = scratch
        for number, handler in old_handlers.items():
            signal.signal(number, handler)


def render(result):
    lines = ["red check (advisory, not admission evidence): do the patch's tests fail on base %s?"
             % result["base"]]
    if result.get("note"):
        lines.append("  " + result["note"])
    for entry in result["files"]:
        detail = " - %s" % entry["detail"] if entry["result"] == NOT_RUN and entry.get("detail") else ""
        lines.append("  %s: %s%s" % (entry["path"], entry["result"], detail))
    if not result["cleaned"]:
        lines.append("  scratch worktree left at %s (oms cleanup --apply removes it)" % result["leftover"])
    return lines


def summary(result):
    counts = {name: sum(1 for entry in result["files"] if entry["result"] == name)
              for name in (FAILS, PASSES, NOT_RUN)}
    if not result["files"]:
        return "%s; advisory, not admission evidence" % result.get("note", "no test file")
    return ("%d test file(s): %d fail on base (good), %d pass on base (do not test the change), "
            "%d could not run; advisory, not admission evidence"
            % (len(result["files"]), counts[FAILS], counts[PASSES], counts[NOT_RUN]))


def main(argv):
    parser = argparse.ArgumentParser(prog="red_check.py")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--patch", required=True)
    parser.add_argument("--verify", required=True)
    parser.add_argument("--base", default="HEAD")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)
    parser.add_argument("--max-files", type=int, default=DEFAULT_MAX_FILES)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        with open(args.patch, "rb") as handle:
            patch = handle.read()
    except OSError as error:
        print("error: %s" % error, file=sys.stderr)
        return 2
    result = check(os.path.realpath(args.repo), patch, args.verify, args.base, args.timeout, args.max_files)
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print("\n".join(render(result)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
