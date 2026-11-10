"""Read-only task routing advice: task kind x risk x size -> recommended tier and effort.

Rules live in config/task-routing-rules.json; the first matching row wins and a final default row
catches the rest. Nothing here launches a model or overrides an explicit model, tier or effort.
Risk reuses the review gate's path selectors (panel_review) so there is one risk-path list.
"""

import itertools
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RULES_PATH = ROOT / "config/task-routing-rules.json"
KINDS = ("explore", "mechanical-edit", "implement", "debug", "design", "review", "summarize")
RISKS = ("low", "medium", "high")
SIZES = ("small", "medium", "large")
EFFORTS = ("low", "medium", "high")
# Frontier stays reserved for security audits (panel_routing), so advice never names it.
TIERS = ("light", "routine", "deep")
AXES = {"kind": KINDS, "risk": RISKS, "size": SIZES}
MAX_PATCH = 4 * 1024 * 1024


def load(path=None):
    return json.loads(Path(path or RULES_PATH).read_text(encoding="utf-8"))


def _match_values(row, axis):
    value = row.get(axis)
    return AXES[axis] if value is None else tuple(value)


def lint(table):
    """Return a list of problems; an empty list means the table is valid."""
    problems = []
    if not isinstance(table, dict) or set(table) != {"schema", "size_buckets", "rules"} or table.get("schema") != 1:
        return ["table must have exactly schema (1), size_buckets and rules"]
    buckets = table["size_buckets"]
    if (not isinstance(buckets, dict) or set(buckets) != {"small", "medium"}
            or any(not isinstance(b, dict) or set(b) != {"max_lines", "max_files"}
                   or any(type(n) is not int or n < 1 for n in b.values()) for b in buckets.values())):
        problems.append("size_buckets needs small and medium with positive integer max_lines and max_files")
    elif any(buckets["small"][k] >= buckets["medium"][k] for k in ("max_lines", "max_files")):
        problems.append("size_buckets small limits must be below medium")
    rules = table["rules"]
    if not isinstance(rules, list) or not rules:
        return problems + ["rules must be a non-empty list"]
    reached = [False] * len(rules)
    for index, row in enumerate(rules):
        label = "rule %d" % (index + 1)
        if not isinstance(row, dict):
            problems.append("%s is not an object" % label)
            continue
        default = row.get("default") is True
        allowed = {"tier", "effort", "reason"} | ({"default"} if default else set(AXES))
        if set(row) - allowed or "default" in row and row["default"] is not True:
            problems.append("%s has unknown keys: %s" % (label, ", ".join(sorted(set(row) - allowed)) or "default"))
        if row.get("tier") not in TIERS:
            problems.append("%s tier must be one of %s" % (label, "/".join(TIERS)))
        if row.get("effort") not in EFFORTS:
            problems.append("%s effort must be one of %s" % (label, "/".join(EFFORTS)))
        if not isinstance(row.get("reason"), str) or not row["reason"].strip():
            problems.append("%s needs a reason" % label)
        if default:
            if index != len(rules) - 1:
                problems.append("%s: default row must be last (later rows are unreachable)" % label)
            continue
        for axis in AXES:
            value = row.get(axis)
            if value is not None and (not isinstance(value, list) or not value or len(set(value)) != len(value)
                                      or any(item not in AXES[axis] for item in value)):
                problems.append("%s %s must be a list of distinct values from %s" % (label, axis, "/".join(AXES[axis])))
    if not isinstance(rules[-1], dict) or rules[-1].get("default") is not True:
        problems.append("missing default row: the last row must be {\"default\": true, ...}")
    if problems:
        return problems
    seen = set()
    for combo in itertools.product(*AXES.values()):
        for index, row in enumerate(rules):
            if row.get("default") or all(combo[n] in _match_values(row, axis) for n, axis in enumerate(AXES)):
                seen.add(index)
                break
    for index in range(len(rules)):
        if index not in seen:
            problems.append("rule %d is unreachable: earlier rows already match everything it matches" % (index + 1))
    return problems


def match(table, kind, risk, size):
    for index, row in enumerate(table["rules"]):
        if row.get("default") or (kind in _match_values(row, "kind") and risk in _match_values(row, "risk")
                                  and size in _match_values(row, "size")):
            return index + 1, row
    raise ValueError("no default row")


def escalate(tier, effort, failed):
    """Raise effort one step, then the tier; a suggestion only."""
    if failed < 2:
        return None
    if effort != "high":
        return {"tier": tier, "effort": EFFORTS[EFFORTS.index(effort) + 1], "step": "effort"}
    if tier != TIERS[-1]:
        return {"tier": TIERS[TIERS.index(tier) + 1], "effort": effort, "step": "tier"}
    return {"tier": tier, "effort": effort, "step": "ceiling"}


def size_bucket(table, lines, files):
    for name in ("small", "medium"):
        limit = table["size_buckets"][name]
        if lines <= limit["max_lines"] and files <= limit["max_files"]:
            return name
    return "large"


def _git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, timeout=30, check=False)


def inspect_range(repo, rng, table):
    """Size bucket and risk class of a git diff range, from git alone."""
    if not rng or rng.startswith("-"):
        raise ValueError("invalid diff range")
    stat = _git(repo, "diff", "--numstat", "--no-renames", rng, "--")
    if stat.returncode != 0:
        raise ValueError("git diff failed for %s" % rng)
    paths, lines = [], 0
    for row in stat.stdout.decode("utf-8", "replace").splitlines():
        parts = row.split("\t")
        if len(parts) < 3:
            continue
        lines += sum(int(n) for n in parts[:2] if n.isdigit())
        paths.append(parts[-1])
    patch = _git(repo, "diff", rng, "--").stdout[:MAX_PATCH]
    from panel_review import _automatic_review_workload, _security_audit_paths
    if not paths:
        risk = "low"
    elif _security_audit_paths(paths, patch):
        risk = "high"
    elif _automatic_review_workload({"changed_files": paths}) == "routine":
        risk = "low"
    else:
        risk = "medium"
    return {"risk": risk, "size": size_bucket(table, lines, len(paths)), "lines": lines, "files": len(paths)}


def cli(args, error):
    """`oms panel --recommend KIND`: print the matched row; never launches anything."""
    if args.recommend not in KINDS:
        error("--recommend must be one of: " + ", ".join(KINDS))
    if (args.risk or args.size) and args.diff or not (args.diff or (args.risk and args.size)):
        error("--recommend needs --diff RANGE, or both --risk and --size")
    if args.failed is not None and args.failed < 0:
        error("--failed must be zero or more")
    problems = lint(table := load())
    if problems:
        error("task routing rules are invalid: " + "; ".join(problems))
    measured = {}
    if args.diff:
        try:
            measured = inspect_range(args.repo or os.getcwd(), args.diff, table)
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            error(str(exc))
    risk, size = args.risk or measured["risk"], args.size or measured["size"]
    number, row = match(table, args.recommend, risk, size)
    out = {"schema": 1, "kind": "oms-route-recommendation", "task_kind": args.recommend, "risk": risk, "size": size,
           "rule": number, "default": bool(row.get("default")), "tier": row["tier"], "effort": row["effort"],
           "reason": row["reason"], "executes": False}
    if measured:
        out["diff"] = {"range": args.diff, "lines": measured["lines"], "files": measured["files"]}
    explicit = {key: value for key, value in (("model", args.model), ("effort", args.reasoning_effort),
                                              ("tier", args.workload)) if value}
    if explicit:
        out["explicit"] = explicit
        out["note"] = "explicit choices are kept; this row is advice only"
    if args.failed is not None:
        hint = escalate(explicit.get("tier", row["tier"]), explicit.get("effort", row["effort"]), args.failed)
        out["failed"] = args.failed
        out["escalation"] = dict(hint, suggestion=True) if hint else None
    if args.json:
        print(json.dumps(out, indent=2))
        return 0
    print("recommend %s (risk %s, size %s): tier %s, effort %s  [rule %d%s]" % (
        args.recommend, risk, size, row["tier"], row["effort"], number, ", default" if out["default"] else ""))
    print("reason: " + row["reason"])
    if explicit:
        print("explicit " + ", ".join("%s=%s" % item for item in explicit.items()) + " is kept; this is advice only")
    if args.failed is not None:
        hint = out["escalation"]
        if hint is None:
            print("escalation: %d failure(s); the hint starts at 2" % args.failed)
        elif hint["step"] == "ceiling":
            print("escalation (suggestion): %d failures at deep/high; no higher tier is advised (frontier is reserved for security audits): narrow the task or ask the owner" % args.failed)
        else:
            print("escalation (suggestion): %d failures; try %s effort at tier %s%s" % (
                args.failed, hint["effort"], hint["tier"], "" if hint["step"] == "effort" else " (effort is already at max, so the tier goes up)"))
    return 0


if __name__ == "__main__":
    import sys
    found = lint(load(sys.argv[1] if len(sys.argv) > 1 else None))
    print("\n".join(found) if found else "ok")
    sys.exit(1 if found else 0)
