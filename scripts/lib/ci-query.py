#!/usr/bin/env python3
"""Bounded, exact-commit GitHub workflow queries shared by release paths.

ci-query.py DESTINATION SHA SECONDS BRANCH WORKFLOW
    prints "RUN_ID CONCLUSION" for the newest push run of SHA, "0 missing" if none.
ci-query.py dispatch DESTINATION SHA SECONDS BRANCH WORKFLOW
    the same for the newest manually dispatched (workflow_dispatch) run of SHA
    on BRANCH; test.yml runs the full selection for every dispatch.
ci-query.py slug DESTINATION
    prints OWNER/REPO for DESTINATION without credentials.
ci-query.py scope DESTINATION RUN_ID SHA SECONDS
    prints "full" when that successful run executed the full selection,
    "affected" when it ran the affected selection only; anything it cannot
    prove exits 2.
"""
import json
import math
import os
import re
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

MAX_QUERY_BYTES = 1024 * 1024
# A run id is read for scope whichever of these events started it.
RUN_EVENTS = ("push", "workflow_dispatch")
PUBLIC_REPO = "eightmm/oh-my-setting"
PUBLIC_WORKFLOW = ".github/workflows/test.yml"

# test.yml's full selection: lint, four focused lanes, four smoke shards, the
# Linux copy lifecycle, the selector and the stable gate. An affected-only run
# reports these matrix jobs once, skipped, under their unexpanded names.
# source-distribution-smoke pins the four-lane/four-shard shape.
FULL_JOBS = (["lint", "affected_plan", "gate", "native (ubuntu-latest, copy)"]
             + ["focused (%d/4)" % lane for lane in range(1, 5)]
             + ["scripts-smoke (%d/4)" % shard for shard in range(1, 5)])
# Native-risk legs and the affected-only operator and narrow-check jobs run only when the plan asks.
OPTIONAL_JOBS = re.compile(
    r"python311|native \((macos|windows)-latest, auto\)|operator-tools \(affected\)|affected checks")


def repo_slug(destination):
    destination = re.sub(r"^[a-z]+://", "", destination)
    destination = re.sub(r"^[^@/]+@", "", destination).replace(":", "/")
    return re.sub(r"\.git/?$", "", destination).rstrip("/")


def public_destination(destination):
    # Never turn an arbitrary, private, credential-bearing or enterprise remote
    # into a public API request. These are the normal spellings of our one repo.
    return re.fullmatch(
        r"(?:https://github\.com/|ssh://git@github\.com/|git@github\.com:|github\.com/)?"
        r"eightmm/oh-my-setting(?:\.git)?/?", destination) is not None


def bounded_call(call, seconds):
    """Bound wall time, including DNS/slow reads, without retaining large output."""
    result = {}
    done = threading.Event()

    def invoke():
        try:
            result["value"] = call()
        except Exception as exc:  # Pass the original exception back to the caller.
            result["error"] = exc
        finally:
            done.set()

    # A timed-out HTTP reader cannot hold this short-lived CLI open. Its socket
    # also has a timeout; no further query is made after this request times out.
    threading.Thread(target=invoke, daemon=True).start()
    if not done.wait(max(0, seconds)):
        raise TimeoutError
    if "error" in result:
        raise result["error"]
    return result["value"]


def stop_process(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def gh(args, seconds):
    """Return (rc, stdout); rc is nonzero after a printed, credential-free reason."""
    cmd = [os.environ.get("OMS_GH_BIN", "gh")] + args
    process = None
    try:
        timeout = min(30.0, max(0.001, float(seconds)))
        deadline = time.monotonic() + timeout
        process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   start_new_session=True)

        def read_output():
            with process.stdout as stream:
                return stream.read(MAX_QUERY_BYTES + 1)

        output = bounded_call(read_output, max(0, deadline - time.monotonic()))
        if len(output) > MAX_QUERY_BYTES:
            stop_process(process)
            print("CI query returned oversized evidence", file=sys.stderr)
            return 2, ""
        process.wait(timeout=max(0.001, deadline - time.monotonic()))
    except (subprocess.TimeoutExpired, TimeoutError):
        stop_process(process)
        print("CI query timed out", file=sys.stderr)
        return 124, ""
    except OSError:
        if process is not None:
            stop_process(process)
        print("CI query unavailable: gh could not start", file=sys.stderr)
        return 127, ""
    if process.returncode:
        # Do not copy credential-bearing remote URLs from a CLI error to receipts.
        print("CI query unavailable: check gh authentication, access and connectivity", file=sys.stderr)
        return process.returncode, ""
    try:
        return 0, output.decode("utf-8")
    except UnicodeDecodeError:
        print("CI query returned invalid text", file=sys.stderr)
        return 2, ""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def public_json(path, seconds):
    """Read only the public OMS API; never use tokens, CLI auth or redirects."""
    timeout = min(30.0, max(0, seconds))
    if timeout <= 0:
        return 124, None
    request = urllib.request.Request(
        "https://api.github.com/repos/" + PUBLIC_REPO + "/" + path,
        headers={"Accept": "application/vnd.github+json", "User-Agent": "oms-ci-query",
                 "X-GitHub-Api-Version": "2022-11-28"})

    def read_response():
        opener = urllib.request.build_opener(NoRedirect())
        with opener.open(request, timeout=timeout) as response:
            raw = response.read(MAX_QUERY_BYTES + 1)
            if len(raw) > MAX_QUERY_BYTES:
                raise ValueError("oversized response")
            return json.loads(raw), response.headers.get("Link", "")

    try:
        return 0, bounded_call(read_response, timeout)
    except TimeoutError:
        print("CI query unavailable: public API timed out", file=sys.stderr)
        return 124, None
    except urllib.error.HTTPError as exc:
        print("CI query unavailable: public API HTTP %s" % exc.code, file=sys.stderr)
        return 1, None
    except (OSError, urllib.error.URLError):
        print("CI query unavailable: public API connectivity failed", file=sys.stderr)
        return 1, None
    except (ValueError, UnicodeError):
        print("CI query returned invalid or oversized public API evidence", file=sys.stderr)
        return 2, None


def public_run(row, sha, run_id=None, branch="", events=("push",)):
    if (not isinstance(row, dict) or row.get("head_sha") != sha
            or row.get("event") not in events or row.get("path") != PUBLIC_WORKFLOW
            or row.get("repository", {}).get("full_name") != PUBLIC_REPO
            or type(row.get("id")) is not int or row["id"] <= 0
            or (run_id is not None and row["id"] != int(run_id))
            or (branch and row.get("head_branch") != branch)):
        raise ValueError("mismatched public run")
    return {"databaseId": row["id"], "headSha": row["head_sha"],
            "status": row["status"], "conclusion": row["conclusion"]}


def query(destination, sha, args, seconds, branch="", workflow="test.yml", run_id=None,
          event="push"):
    """Keep gh first; a failed client may use bounded public OMS evidence."""
    deadline = time.monotonic() + min(30.0, max(0, seconds))
    rc, output = gh(args, seconds)
    if not rc or rc in (2, 124) or not public_destination(destination) \
            or workflow not in ("test.yml", PUBLIC_WORKFLOW) \
            or not re.fullmatch(r"[0-9a-f]{40}", sha):
        return rc, output
    params = {"head_sha": sha, "event": event, "per_page": 1}
    if branch:
        params["branch"] = branch
    path = ("actions/runs/" + run_id if run_id else
            "actions/workflows/test.yml/runs?" + urllib.parse.urlencode(params))
    rc, payload = public_json(path, deadline - time.monotonic())
    if rc:
        return rc, ""
    try:
        data, link = payload
        if run_id is None:
            rows = data["workflow_runs"]
            if not isinstance(rows, list) or len(rows) > 1 \
                    or type(data["total_count"]) is not int or data["total_count"] < len(rows) \
                    or (not rows and data["total_count"]):
                raise ValueError("invalid run list")
            return 0, json.dumps([public_run(row, sha, branch=branch, events=(event,))
                                  for row in rows])
        run = public_run(data, sha, run_id, events=RUN_EVENTS)
        attempt = data["run_attempt"]
        if type(attempt) is not int or attempt <= 0:
            raise ValueError("invalid run attempt")
        rc, payload = public_json("actions/runs/%s/attempts/%s/jobs?per_page=100" %
                                  (run_id, attempt), deadline - time.monotonic())
        if rc:
            return rc, ""
        jobs, link = payload
        rows = jobs["jobs"]
        if not isinstance(rows, list) or len(rows) > 100 \
                or type(jobs["total_count"]) is not int or jobs["total_count"] != len(rows) \
                or re.search(r'rel="?next\b', link):
            raise ValueError("truncated jobs")
        if any(type(row["run_id"]) is not int or row["run_id"] != int(run_id)
               or row["head_sha"] != sha or type(row["run_attempt"]) is not int
               or row["run_attempt"] != attempt for row in rows):
            raise ValueError("mismatched jobs")
        run["jobs"] = rows
        return 0, json.dumps(run)
    except (ValueError, KeyError, TypeError, AttributeError):
        print("CI query returned invalid or mismatched public API evidence", file=sys.stderr)
        return 2, ""


def run_conclusion(run, sha):
    """One exact-SHA conclusion contract for both eligibility and scope reads."""
    if run["headSha"] != sha or run["status"] not in (
            "completed", "queued", "in_progress", "pending", "waiting", "requested"):
        raise ValueError("run identity or status is invalid")
    conclusion = run["conclusion"] if run["status"] == "completed" else "pending"
    if not isinstance(conclusion, str) or not re.fullmatch(r"[a-z_]+", conclusion):
        raise ValueError("run conclusion is invalid")
    return conclusion


def latest_run(destination, sha, seconds, branch, workflow, event="push"):
    args = ["run", "list", "--repo", repo_slug(destination), "--commit", sha, "--event", event,
            "--limit", "1", "--json", "databaseId,status,conclusion,headSha"]
    if branch:
        args += ["--branch", branch]
    if workflow:
        args += ["--workflow", workflow]
    try:
        budget = max(0.0, float(seconds))
        if not math.isfinite(budget):
            raise ValueError
    except ValueError:
        print("CI query wait budget is invalid", file=sys.stderr)
        return 2
    deadline = time.monotonic() + budget
    pending_id = None
    while True:
        remaining = max(0.0, deadline - time.monotonic())
        # Only an explicit zero budget gets a full single-request timeout.
        rc, output = query(destination, sha, args, remaining if budget > 0 else 30,
                           branch, workflow, event=event)
        if rc:
            if rc == 124 and budget > 0 and pending_id is not None:
                print(pending_id, "pending")
                return 0
            return rc
        try:
            rows = json.loads(output)
            if not isinstance(rows, list) or len(rows) > 1:
                raise ValueError
            if not rows:
                print("0 missing")
                return 0
            row = rows[0]
            if type(row["databaseId"]) is not int or row["databaseId"] <= 0:
                raise ValueError
            conclusion = run_conclusion(row, sha)
            if row["status"] == "completed":
                print(row["databaseId"], conclusion)
                return 0
            pending_id = row["databaseId"]
            if budget <= 0:
                print(row["databaseId"], "pending")
                return 0
            remaining = max(0.0, deadline - time.monotonic())
            if remaining <= 0:
                print(row["databaseId"], "pending")
                return 0
        except (ValueError, KeyError, TypeError):
            print("CI query returned invalid or mismatched evidence", file=sys.stderr)
            return 2
        time.sleep(min(10, max(0, deadline - time.monotonic())))
        if time.monotonic() >= deadline:
            print(row["databaseId"], "pending")
            return 0


def run_scope(destination, run_id, sha, seconds):
    if not re.fullmatch(r"[1-9][0-9]*", run_id):
        print("CI scope needs a run id", file=sys.stderr)
        return 2
    try:
        budget = max(0.0, float(seconds))
        if not math.isfinite(budget):
            raise ValueError
    except ValueError:
        print("CI query wait budget is invalid", file=sys.stderr)
        return 2
    deadline = time.monotonic() + budget
    args = ["run", "view", run_id, "--repo", repo_slug(destination),
            "--json", "headSha,status,conclusion,jobs"]
    while True:
        remaining = max(0.0, deadline - time.monotonic())
        rc, output = query(destination, sha, args, remaining if budget > 0 else 30,
                           run_id=run_id)
        if rc:
            return rc
        try:
            run = json.loads(output)
            if run_conclusion(run, sha) != "success" or not isinstance(run["jobs"], list):
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
        if not unsettled or deadline - time.monotonic() < 7:
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
    if sys.argv[1:2] == ["dispatch"] and len(sys.argv) == 7:
        return latest_run(*sys.argv[2:], event="workflow_dispatch")
    if sys.argv[1:2] == ["slug"] and len(sys.argv) == 3:
        print(repo_slug(sys.argv[2]))
        return 0
    if len(sys.argv) != 6:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    return latest_run(*sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
