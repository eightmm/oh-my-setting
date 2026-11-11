#!/usr/bin/env python3
"""Advice on how deep to review a diff. It prints; it never starts a review.

The depth is one of none, single, gate+security or council. The inputs are the
risk paths of the diff (the selector the security seat and patch-admit already
share, never a second list), the contract and public-surface files named in
config/review-depth.json, and the diff's size. The thresholds live in that file.
Nothing here calls a model, writes a file or moves a gate verdict.
"""

import argparse
import fnmatch
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

ROOT = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
LIMITS = ("none_lines", "gate_lines", "gate_files", "council_lines", "council_files")
MAX_PATCH = 64 * 1024 * 1024


def load_config(path):
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
    globs = data.get("contract_paths") if isinstance(data, dict) else None
    if (not isinstance(data, dict) or data.get("schema") != 1
            or not isinstance(globs, list) or not all(isinstance(item, str) for item in globs)
            or any(type(data.get(key)) is not int or data[key] < 0 for key in LIMITS)):
        raise ValueError("invalid review depth config: %s" % path)
    return data


def git(repo, *args):
    env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
    env.pop("GIT_DIR", None)
    env.pop("GIT_WORK_TREE", None)
    run = subprocess.run(["git", "-c", "core.fsmonitor=false", "-c", "diff.external=", "-C", repo,
                          "diff", "--no-ext-diff", "--no-textconv", "--no-renames", *args],
                         capture_output=True, env=env, check=False)
    if run.returncode != 0:
        raise ValueError("git diff failed")
    return run.stdout


def measure(repo, base):
    """(changed paths, added+deleted lines, patch bytes) of the tracked diff against base."""
    files, lines = [], 0
    for row in git(repo, "--numstat", base, "--").decode("utf-8", "replace").splitlines():
        added, deleted, path = (row.split("\t", 2) + ["", "", ""])[:3]
        files.append(path)
        lines += (int(added) if added.isdigit() else 0) + (int(deleted) if deleted.isdigit() else 0)
    patch = git(repo, base, "--")
    return files, lines, patch if len(patch) <= MAX_PATCH else b""


def advise(config, files, lines, patch):
    from panel_review import _security_audit_paths
    risk = _security_audit_paths(files, patch)
    contract = sorted(path for path in files if any(fnmatch.fnmatchcase(path, glob)
                                                    for glob in config["contract_paths"]))
    reasons, depth = [], "none"

    def raise_to(level, reason):
        nonlocal depth
        order = ("none", "single", "gate+security", "council")
        if order.index(level) > order.index(depth):
            depth = level
        reasons.append("%s: %s" % (level, reason))

    if files and (risk or contract or lines > config["none_lines"]):
        raise_to("single", "%d line(s) in %d file(s)" % (lines, len(files)))
    if risk:
        raise_to("gate+security", "risk path: %s" % ", ".join(risk[:5]))
    if contract:
        raise_to("gate+security", "contract file: %s" % ", ".join(contract[:5]))
    if lines >= config["gate_lines"] or len(files) >= config["gate_files"]:
        raise_to("gate+security", "size %d lines, %d files (>= %d lines or %d files)"
                 % (lines, len(files), config["gate_lines"], config["gate_files"]))
    if risk and contract:
        raise_to("council", "risk path and contract file in one change")
    if lines >= config["council_lines"] or len(files) >= config["council_files"]:
        raise_to("council", "size %d lines, %d files (>= %d lines or %d files)"
                 % (lines, len(files), config["council_lines"], config["council_files"]))
    if not files:
        reasons.append("none: no tracked change against the base")
    elif depth == "none":
        reasons.append("none: %d line(s), no risk or contract path" % lines)
    return {"schema": 1, "advisory": True, "depth": depth, "reasons": reasons,
            "risk_paths": risk[:50], "contract_paths": contract[:50],
            "lines": lines, "files": len(files)}


def main(argv):
    parser = argparse.ArgumentParser(prog="review_depth.py")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--base", default="HEAD")
    parser.add_argument("--config", default=os.path.join(ROOT, "config", "review-depth.json"))
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--red-check", metavar="CMD", default="")
    parser.add_argument("--red-timeout", type=int, default=300)
    args = parser.parse_args(argv)
    try:
        result = advise(load_config(args.config), *measure(args.repo, args.base))
        red = None
        if args.red_check:
            import red_check
            red = red_check.check(os.path.realpath(args.repo), git(args.repo, "--binary", args.base, "--"),
                                  args.red_check, args.base, args.red_timeout)
    except (OSError, ValueError) as error:
        print("error: %s" % error, file=sys.stderr)
        return 2
    if red is not None:
        result["red_check"] = red
    if args.json:
        print(json.dumps(result, sort_keys=True))
        return 0
    print("review depth: %s" % result["depth"])
    for reason in result["reasons"]:
        print("  - %s" % reason)
    if red is not None:
        print("\n".join(red_check.render(red)))
    print("advice only: this starts no review and changes no gate verdict")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
