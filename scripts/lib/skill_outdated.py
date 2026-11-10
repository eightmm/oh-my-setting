#!/usr/bin/env python3
"""Read-only check of whether an imported skill bundle's upstream moved.

For every `.oms/skill-store/NAME/lock.json` whose provenance is a git source,
ask the remote where the recorded ref points now (`git ls-remote`, nothing
else) and compare it with the revision the import resolved. Nothing is
fetched, cloned, stored or applied, and nothing calls this on a schedule.

Three answers per bundle, never two: `current`, `moved`, or `unknown` with a
fixed reason. An unreachable remote is unknown, not current.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

MAX_LOCK_BYTES = 262144
MAX_OUTPUT_BYTES = 65536
MAX_BUNDLES = 200
SHA_RE = re.compile(r"^[0-9a-f]{40}([0-9a-f]{24})?$")
NAME_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")


def timeout_seconds() -> int:
    raw = os.environ.get("OMS_SKILL_OUTDATED_TIMEOUT", "20")
    try:
        value = int(raw)
    except ValueError:
        value = 20
    return max(1, min(120, value))


def repo_root(raw: str) -> Path:
    path = Path(raw).resolve()
    try:
        done = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            timeout=10, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return path
    top = done.stdout.decode("utf-8", errors="replace").strip()
    return Path(top).resolve() if done.returncode == 0 and top else path


def read_lock(path: Path) -> Optional[Dict[str, Any]]:
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_LOCK_BYTES:
            return None
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, UnicodeError):
        return None
    if not isinstance(value, dict) or not isinstance(value.get("provenance"), dict):
        return None
    return value


def remote_revision(locator: str, reference: str) -> Tuple[Optional[str], str]:
    """(revision, reason): the revision `reference` names on the remote now, or None and why not."""
    env = dict(os.environ)
    env.update({"GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1"})
    wanted = reference or "HEAD"
    # The import checks a ref out of its own clone, where a remote branch is spelled origin/NAME.
    if wanted.startswith("origin/") and len(wanted) > 7:
        wanted = wanted[7:]
    try:
        done = subprocess.run(
            ["git", "-c", "core.hooksPath=/dev/null", "ls-remote", "--", locator, wanted],
            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            timeout=timeout_seconds(), check=False,
        )
    except subprocess.TimeoutExpired:
        return None, "remote did not answer in time"
    except OSError:
        return None, "git is not runnable"
    if done.returncode != 0:
        return None, "remote could not be read"
    if len(done.stdout) > MAX_OUTPUT_BYTES:
        return None, "remote answer too large"
    rows: List[Tuple[str, str]] = []
    for line in done.stdout.decode("utf-8", errors="replace").splitlines():
        parts = line.split("\t")
        if len(parts) == 2 and SHA_RE.match(parts[0]):
            rows.append((parts[1], parts[0]))
    if not rows:
        return None, "the recorded ref is no longer on the remote"
    names = dict(rows)
    # An annotated tag lists the tag object and, peeled, the commit it names: the import resolved the commit.
    for candidate in ("HEAD" if wanted == "HEAD" else "", "refs/heads/" + wanted, "refs/tags/" + wanted + "^{}",
                      "refs/tags/" + wanted, wanted):
        if candidate and candidate in names:
            return names[candidate], ""
    if len(rows) == 1:
        return rows[0][1], ""
    return None, "the recorded ref is ambiguous on the remote"


def inspect(repo: Path) -> Dict[str, Any]:
    store = repo / ".oms" / "skill-store"
    bundles: List[Dict[str, Any]] = []
    omitted = 0
    names: List[str] = []
    if store.is_dir() and not store.is_symlink():
        try:
            names = sorted(entry.name for entry in store.iterdir() if entry.is_dir() and not entry.is_symlink())
        except OSError:
            names = []
    for name in names:
        if not NAME_RE.match(name):
            continue
        if len(bundles) >= MAX_BUNDLES:
            omitted += 1
            continue
        row: Dict[str, Any] = {"name": name, "status": "unknown"}
        lock = read_lock(store / name / "lock.json")
        if lock is None:
            row["reason"] = "lock receipt is unreadable"
            bundles.append(row)
            continue
        provenance = lock["provenance"]
        kind = provenance.get("kind")
        recorded = provenance.get("resolved_revision")
        reference = provenance.get("requested_ref") or ""
        locator = provenance.get("locator")
        row["kind"] = kind if isinstance(kind, str) else "unknown"
        if kind != "git":
            # A local import records a digest of its path, not the path, so there is nothing to ask.
            row["reason"] = "imported from a local directory; no upstream is recorded"
        elif not (isinstance(locator, str) and locator and isinstance(recorded, str) and SHA_RE.match(recorded)
                  and isinstance(reference, str)):
            row["reason"] = "lock receipt records no usable git source"
        elif locator.startswith("-") or reference.startswith("-"):
            row["reason"] = "lock receipt records no usable git source"
        elif SHA_RE.match(reference):
            row.update(status="current", recorded=recorded, reason="pinned to a commit; a pin cannot move")
        else:
            row["ref"] = reference or "HEAD"
            row["recorded"] = recorded
            revision, reason = remote_revision(locator, reference)
            if revision is None:
                row["reason"] = reason
            else:
                row["remote"] = revision
                row["status"] = "current" if revision == recorded else "moved"
        bundles.append(row)
    counts = {key: sum(1 for row in bundles if row["status"] == key) for key in ("current", "moved", "unknown")}
    return {"schema": 1, "kind": "oms-skill-outdated", "bundles": bundles, "counts": counts, "omitted": omitted}


def render(report: Dict[str, Any]) -> str:
    lines: List[str] = []
    bundles = report["bundles"]
    if not bundles:
        return "skill-forge outdated: no imported bundles\n"
    for row in bundles:
        detail = ""
        if row["status"] == "moved":
            detail = "%s: %s -> %s" % (row.get("ref", "HEAD"), row["recorded"][:12], row["remote"][:12])
        elif row["status"] == "current":
            detail = row.get("reason") or "%s at %s" % (row.get("ref", "HEAD"), row["recorded"][:12])
        else:
            detail = row.get("reason", "")
        lines.append("%-8s %s  %s" % (row["status"], row["name"], detail))
    counts = report["counts"]
    lines.append("skill-forge outdated: %d moved, %d current, %d unknown; nothing was fetched or applied"
                 % (counts["moved"], counts["current"], counts["unknown"]))
    if counts["moved"]:
        lines.append("  to look at one: oms skill-forge preview --source SOURCE [--ref REF] --json, then update")
    if counts["unknown"]:
        lines.append("  unknown is not current: nothing here shows those bundles match their upstream")
    if report["omitted"]:
        lines.append("  %d more bundle(s) were not checked (limit %d)" % (report["omitted"], MAX_BUNDLES))
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(prog="skill-forge outdated")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    report = inspect(repo_root(args.repo))
    if args.json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    else:
        sys.stdout.write(render(report))
    if report["counts"]["moved"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
