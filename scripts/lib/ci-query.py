#!/usr/bin/env python3
"""One bounded, exact-commit GitHub workflow query shared by release paths."""
import json
import os
import re
import signal
import subprocess
import sys


def main():
    destination, sha, seconds, branch, workflow = sys.argv[1:]
    destination = re.sub(r"^[a-z]+://", "", destination)
    destination = re.sub(r"^[^@/]+@", "", destination).replace(":", "/")
    destination = re.sub(r"\.git/?$", "", destination).rstrip("/")
    cmd = [os.environ.get("OMS_GH_BIN", "gh"), "run", "list", "--repo", destination,
           "--commit", sha, "--event", "push", "--limit", "1",
           "--json", "databaseId,status,conclusion,headSha"]
    if branch:
        cmd += ["--branch", branch]
    if workflow:
        cmd += ["--workflow", workflow]
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
        return 124
    except OSError:
        print("CI query unavailable: gh could not start", file=sys.stderr)
        return 127
    if process.returncode:
        # Do not copy credential-bearing remote URLs from a CLI error to receipts.
        print("CI query failed: check gh authentication, access and connectivity", file=sys.stderr)
        return process.returncode
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


if __name__ == "__main__":
    raise SystemExit(main())
