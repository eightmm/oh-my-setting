#!/usr/bin/env python3
"""Bounded, exact-commit GitHub workflow queries shared by release paths.

ci-query.py DESTINATION SHA SECONDS BRANCH WORKFLOW
    prints "RUN_ID CONCLUSION" for the newest push run of SHA, "0 missing" if none.
ci-query.py scope DESTINATION RUN_ID SHA SECONDS
    prints "full" when that successful run executed the full selection,
    "affected" when it ran the affected selection only; anything it cannot
    prove exits 2.
"""
import json
import os
import re
import signal
import subprocess
import sys
import time

# test.yml's full selection: lint, four focused lanes, four smoke shards, the
# Linux copy lifecycle, the selector and the stable gate. An affected-only run
# reports these matrix jobs once, skipped, under their unexpanded names.
# source-distribution-smoke pins the four-lane/four-shard shape.
FULL_JOBS = (["lint", "affected_plan", "gate", "native (ubuntu-latest, copy)"]
             + ["focused (%d/4)" % lane for lane in range(1, 5)]
             + ["scripts-smoke (%d/4)" % shard for shard in range(1, 5)])
# Native-risk legs run only when the plan asks for them.
OPTIONAL_JOBS = re.compile(r"python39|native \((macos|windows)-latest, auto\)")


def repo_slug(destination):
    destination = re.sub(r"^[a-z]+://", "", destination)
    destination = re.sub(r"^[^@/]+@", "", destination).replace(":", "/")
    return re.sub(r"\.git/?$", "", destination).rstrip("/")


def gh(args, seconds):
    """Return (rc, stdout); rc is nonzero after a printed, credential-free reason."""
    cmd = [os.environ.get("OMS_GH_BIN", "gh")] + args
    try:
        process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   text=True, start_new_session=os.name == "posix")
        output, _ = process.communicate(timeout=min(30, max(1, int(seconds))))
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            process.kill()
        process.wait()
        print("CI query timed out", file=sys.stderr)
        return 124, ""
    except OSError:
        print("CI query unavailable: gh could not start", file=sys.stderr)
        return 127, ""
    if process.returncode:
        # Do not copy credential-bearing remote URLs from a CLI error to receipts.
        print("CI query failed: check gh authentication, access and connectivity", file=sys.stderr)
        return process.returncode, ""
    return 0, output


def latest_run(destination, sha, seconds, branch, workflow):
    args = ["run", "list", "--repo", repo_slug(destination), "--commit", sha, "--event", "push",
            "--limit", "1", "--json", "databaseId,status,conclusion,headSha"]
    if branch:
        args += ["--branch", branch]
    if workflow:
        args += ["--workflow", workflow]
    rc, output = gh(args, seconds)
    if rc:
        return rc
    try:
        rows = json.loads(output)
        if not isinstance(rows, list) or len(rows) > 1:
            raise ValueError
        if not rows:
            print("0 missing")
            return 0
        row = rows[0]
        if row["headSha"] != sha or type(row["databaseId"]) is not int or row["databaseId"] <= 0:
            raise ValueError
        conclusion = row["conclusion"] if row["status"] == "completed" else "pending"
        if not isinstance(conclusion, str) or not re.fullmatch(r"[a-z_]+", conclusion):
            raise ValueError
        print(row["databaseId"], conclusion)
    except (ValueError, KeyError, TypeError):
        print("CI query returned invalid or mismatched evidence", file=sys.stderr)
        return 2
    return 0


def run_scope(destination, run_id, sha, seconds):
    if not re.fullmatch(r"[1-9][0-9]*", run_id):
        print("CI scope needs a run id", file=sys.stderr)
        return 2
    deadline = time.monotonic() + max(0.0, float(seconds))
    args = ["run", "view", run_id, "--repo", repo_slug(destination),
            "--json", "headSha,status,conclusion,jobs"]
    for attempt in range(3):
        remaining = max(0.0, deadline - time.monotonic())
        rc, output = gh(args, remaining if attempt else seconds)
        if rc:
            return rc
        try:
            run = json.loads(output)
            if (run["headSha"] != sha or run["status"] != "completed"
                    or run["conclusion"] != "success" or not isinstance(run["jobs"], list)):
                raise ValueError("run is not a completed success for this commit")
            results = {}
            for job in run["jobs"]:
                name, conclusion = job["name"], job["conclusion"]
                if not isinstance(name, str) or name in results:
                    raise ValueError("unnamed or duplicate job")
                results[name] = conclusion
        except (ValueError, KeyError, TypeError) as exc:
            print("CI scope evidence is invalid: %s" % exc, file=sys.stderr)
            return 2
        unsettled = any(not isinstance(value, str) or not value.strip()
                        or value in ("queued", "in_progress", "pending", "waiting")
                        for value in results.values())
        if not unsettled or attempt == 2 or deadline - time.monotonic() < 7:
            break
        time.sleep(5)
    if all(results.get(name) == "success" for name in FULL_JOBS):
        extra = [name for name, conclusion in results.items() if name not in FULL_JOBS
                 and not (conclusion == "success"
                          or (conclusion == "skipped" and OPTIONAL_JOBS.fullmatch(name)))]
        if not extra:
            print("full")
            return 0
        details = sorted((name, results[name]) for name in extra)[:5]
        print("CI scope is ambiguous: unexpected job results %s" % details, file=sys.stderr)
        return 2
    if results.get("lint") == "skipped" and results.get("affected_plan") == "success" \
            and results.get("gate") == "success":
        print("affected")
        return 0
    priority_jobs = ["lint", "affected_plan", "gate"] + [
        name for name in FULL_JOBS if name not in ("lint", "affected_plan", "gate")]
    broken = [(name, results[name] if name in results else "missing")
              for name in priority_jobs if results.get(name) != "success"][:5]
    print("CI scope is ambiguous: the full selection's jobs are not all successful %s"
          % broken, file=sys.stderr)
    return 2


def main():
    if sys.argv[1:2] == ["scope"] and len(sys.argv) == 6:
        return run_scope(*sys.argv[2:])
    if len(sys.argv) != 6:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    return latest_run(*sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
