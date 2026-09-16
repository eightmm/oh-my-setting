"""Bounded historical replay of repair stopping rules; never execute a candidate.

Inspired by Dream-RSI (arxiv:2609.14858), not its unreleased implementation.
The fixed incumbent spends the requested repair budget. The challenger stops
after an identical failed candidate and diagnostic recur consecutively.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re

from peer_artifacts import tail_lines


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def valid_trace(value: object) -> bool:
    if not isinstance(value, dict) or set(value) != {
        "schema", "cohort", "budget", "policy", "samples", "steps"
    }:
        return False
    if type(value["schema"]) is not int or value["schema"] != 1 or value["policy"] not in ("budget", "stop-repeat"):
        return False
    if not isinstance(value["cohort"], str) or not re.fullmatch(r"[0-9a-f]{64}", value["cohort"]):
        return False
    if type(value["budget"]) is not int or not 1 <= value["budget"] <= 3:
        return False
    if type(value["samples"]) is not int or not 0 <= value["samples"] <= 1000:
        return False
    steps = value["steps"]
    if not isinstance(steps, list) or not 1 <= len(steps) <= value["budget"] + 1:
        return False
    for step in steps:
        if not isinstance(step, dict) or set(step) != {"candidate", "failure", "worker", "verify"}:
            return False
        if any(type(step[k]) is not int or not 0 <= step[k] <= 255 for k in ("worker", "verify")):
            return False
        if any(not isinstance(step[k], str) or not re.fullmatch(r"[0-9a-f]{64}", step[k])
               for k in ("candidate", "failure")):
            return False
    return True


def passed(step: dict) -> bool:
    return step["worker"] == step["verify"] == 0


def repeated(steps: list[dict]) -> bool:
    return len(steps) >= 2 and not passed(steps[-1]) and steps[-1] == steps[-2]


def replay(steps: list[dict], policy: str) -> tuple[bool, int]:
    # Reveal only a prefix to the policy, not the future successful outcome.
    for count, step in enumerate(steps, 1):
        if passed(step) or (policy == "stop-repeat" and repeated(steps[:count])):
            return passed(step), count
    return passed(steps[-1]), len(steps)


def select(rows: list[dict], cohort: str, budget: int) -> tuple[str, int]:
    latest = {}
    lost_success = False
    challenger_runs = 0
    for row in rows:
        trace = row.get("repair_replay")
        if row.get("kind") != "delegate" or not valid_trace(trace):
            continue
        if row.get("fallback_used") or row.get("model_attribution") == "ambiguous":
            continue
        if trace["cohort"] != cohort or trace["budget"] != budget:
            continue
        steps = trace["steps"]
        if any(s["worker"] in (125, 126, 127) or s["verify"] == 125 for s in steps):
            continue
        if any(passed(s) for s in steps[:-1]):
            continue
        if type(row.get("exit")) is not int or row["exit"] != (0 if passed(steps[-1]) else 1):
            continue
        if type(row.get("verify_exit")) is not int or row["verify_exit"] != steps[-1]["verify"]:
            continue
        identity = row.get("prompt_hash")
        if not isinstance(identity, str) or not re.fullmatch(r"[0-9a-f]{64}", identity):
            continue
        if trace["policy"] == "stop-repeat":
            if passed(steps[-1]) or repeated(steps) or len(steps) == budget + 1:
                challenger_runs += 1
            continue
        # Short failed traces are censored, not evidence that more repair fails.
        if passed(steps[0]) or (not passed(steps[-1]) and len(steps) != budget + 1):
            continue
        challenger_runs = 0
        # Deduplicate sample counts, never discard counterevidence from a rerun.
        lost_success |= passed(steps[-1]) and not replay(steps, "stop-repeat")[0]
        latest.pop(identity, None)
        latest[identity] = steps
    episodes = list(latest.values())
    # Replenish uncensored evidence within the existing cap, without extra runs.
    if len(episodes) < 12 or lost_success or challenger_runs >= 4:
        return "budget", len(episodes)
    split = len(episodes) * 2 // 3
    # Chronological held-out history; a non-regression is empirical, not a
    # guarantee on new tasks. Ties retain the incumbent, including sparse data.
    for partition in (episodes[:split], episodes[split:]):
        saved = 0
        for steps in partition:
            _, base_calls = replay(steps, "budget")
            _, calls = replay(steps, "stop-repeat")
            saved += base_calls - calls
        if saved <= 0:
            return "budget", len(episodes)
    return "stop-repeat", len(episodes)


def history(index: Path) -> list[dict]:
    if not index.exists():
        return []
    lines, truncated = tail_lines(index, 1000, max_bytes=2 * 1024 * 1024)
    if truncated:
        return []
    rows = [json.loads(line) for line in lines if line.strip()]
    if not all(isinstance(row, dict) for row in rows):
        raise ValueError("invalid history")
    return rows


def file_digest(path: str) -> str:
    with open(path, "rb") as handle:
        data = handle.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024:
        raise ValueError("oversized repair evidence")
    return digest(data)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--index", type=Path, required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--provider", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", default="")
    parser.add_argument("--budget", type=int, choices=(1, 2, 3), required=True)
    parser.add_argument("--patch", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--worker", type=int, required=True)
    parser.add_argument("--verify", type=int, required=True)
    args = parser.parse_args()
    try:
        command = os.environ.get("OMS_REPLAY_VERIFY", "")
        cohort = digest(json.dumps([args.base, args.provider, args.model, args.effort,
                                   command], ensure_ascii=True).encode())
        previous = os.environ.get("OMS_REPLAY_TRACE", "")
        if previous:
            trace = json.loads(previous)
            if not valid_trace(trace) or trace["cohort"] != cohort or trace["budget"] != args.budget:
                raise ValueError("changed replay context")
        else:
            # Provider defaults can silently change models between executions.
            policy, samples = "budget", 0
            if (args.budget > 1 and args.model and args.model not in ("default", "provider-default")
                    and os.environ.get("OMS_REPAIR_REPLAY", "1") != "0"):
                policy, samples = select(history(args.index), cohort, args.budget)
            trace = dict(schema=1, cohort=cohort, budget=args.budget, policy=policy,
                         samples=samples, steps=[])
        trace["steps"].append(dict(candidate=file_digest(args.patch), failure=file_digest(args.log),
                                   worker=args.worker, verify=args.verify))
        if not command or not valid_trace(trace):
            raise ValueError("invalid repair evidence")
        stop = trace["policy"] == "stop-repeat" and repeated(trace["steps"])
        print("stop" if stop else "continue")
        print(json.dumps(trace, separators=(",", ":")))
        return 0
    except (OSError, ValueError, TypeError):
        print("repair replay unavailable; retain explicit repair budget", file=os.sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
