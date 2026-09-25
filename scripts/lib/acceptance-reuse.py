"""Opt-in, same-run reuse of a supervised acceptance receipt.

The project asserts snapshot-pure checks and stable external tools. This is not
a cache for network, time, hardware, or mutable ignored inputs.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
from datetime import datetime


def context(repo, runner):
    spec = Path(repo) / "PROJECT.md"
    if spec.is_symlink() or not spec.is_file() or spec.stat().st_size > 1024 * 1024:
        return ""
    raw = spec.read_bytes()
    section = re.search(r"(?ms)^## Verification\s*\n(.*?)(?=^## |\Z)", raw.decode())
    if not section:
        return ""
    declarations = re.findall(r"(?m)^- Acceptance reuse: ([^\r\n]+)", section[1])
    if declarations != ["same-run"]:
        return ""
    # Git's normalized diff can conceal CRLF/clean-filter byte changes.
    # Bind raw tracked bytes separately; oversized trees and submodules are
    # ineligible for reuse, without blocking ordinary fresh acceptance.
    tracked = subprocess.check_output(
        ["git", "-c", "core.fsmonitor=false", "-C", repo, "ls-files", "-z"],
        stderr=subprocess.DEVNULL)
    files = hashlib.sha256()
    remaining = 64 * 1024 * 1024
    entries = []
    for name in sorted(set(tracked.split(b"\0")) - {b""}):
        path = Path(repo) / os.fsdecode(name)
        entries.append((name, path, path.lstat()))
    # Sizes only reject before any read; the streamed budget still binds growth.
    if sum(i.st_size for _, _, i in entries if stat.S_ISREG(i.st_mode)) > remaining:
        return ""
    for name, path, info in entries:
        content = hashlib.sha256()
        if stat.S_ISREG(info.st_mode):
            with path.open("rb") as handle:
                while True:
                    chunk = handle.read(min(1024 * 1024, remaining + 1))
                    if not chunk:
                        break
                    remaining -= len(chunk)
                    if remaining < 0:
                        return ""
                    content.update(chunk)
        elif stat.S_ISLNK(info.st_mode):
            content.update(os.fsencode(os.readlink(path)))
        else:
            return ""
        files.update(name + b"\0" + str(info.st_mode).encode() + b"\0" + content.digest())
    # Execution labels change between a drive and its immediate parent check.
    # Every other environment value is bound, without persisting its contents.
    metadata = {"_", "SHLVL", "PWD", "OLDPWD", "OMS_GOAL_RUN_ID",
                "OMS_GOAL_CYCLE", "OMS_AUTOPILOT_PHASE_WALL", "OMS_TS"}
    environment = {key: value for key, value in os.environ.items() if key not in metadata}
    # The driver appends the same hook suppression already set by autopilot.
    # Collapse only adjacent identical suppression entries, not arbitrary Git
    # config (which may be ordered or multivalued).
    git_config = []
    count = int(environment.pop("GIT_CONFIG_COUNT", "0"))
    if not 0 <= count <= 64:
        return ""
    for index in range(count):
        entry = [environment.pop("GIT_CONFIG_KEY_%d" % index),
                 environment.pop("GIT_CONFIG_VALUE_%d" % index)]
        if entry == ["core.hooksPath", "/dev/null"] and git_config[-1:] == [entry]:
            continue
        git_config.append(entry)
    payload = {"spec": hashlib.sha256(raw).hexdigest(), "files": files.hexdigest(),
               "environment": environment, "git_config": git_config,
               "runner": hashlib.sha256(Path(runner).read_bytes()).hexdigest(),
               "helper": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
               "repo": os.path.realpath(repo)}
    return hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()


def reusable(progress, run_id, context_sha, plan, snapshot, manifest, command):
    if not context_sha or not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", run_id):
        return False
    path = Path(progress)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024 * 1024:
        return False
    latest = None
    for line in path.read_text().splitlines():
        row = json.loads(line)
        if not isinstance(row, dict):
            return False
        if row.get("kind") == "acceptance":
            latest = row
    if not latest:
        return False
    expected = {"schema": 1, "status": "pass", "exit": 0, "reason": "",
                "timed_out": False, "output_limited": False, "run_id": run_id,
                "reuse_context_sha256": context_sha, "plan_sha256": plan,
                "repo_snapshot_sha256": snapshot, "manifest_sha256": manifest,
                "accept_sha256_full": hashlib.sha256(command.encode()).hexdigest()}
    if any(latest.get(key) != value for key, value in expected.items()):
        return False
    age = time.time() - datetime.fromisoformat(latest["ts"].replace("Z", "+00:00")).timestamp()
    if not 0 <= age <= 600:
        return False
    receipt = hashlib.sha256(json.dumps(latest, sort_keys=True).encode()).hexdigest()
    print("plan-accept: reused pass run=%s receipt=%s" % (run_id, receipt))
    return True


if __name__ == "__main__":
    try:
        if sys.argv[1] == "context":
            print(context(*sys.argv[2:]))
        elif sys.argv[1] == "check":
            raise SystemExit(0 if reusable(*sys.argv[2:]) else 1)
        else:
            raise SystemExit(2)
    except (OSError, ValueError, KeyError, TypeError, OverflowError, subprocess.SubprocessError):
        raise SystemExit(1)
