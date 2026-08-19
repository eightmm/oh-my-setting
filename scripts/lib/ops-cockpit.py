#!/usr/bin/env python3
"""Read-only operator summary composed from the existing OMS query surfaces."""

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, List


ROOT = Path(__file__).resolve().parents[2]


def fail(message: str) -> "NoReturn":
    print("error: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def positive(value: str) -> int:
    try:
        number = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("must be a positive integer")
    if number <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def read_surface(script: str, args: List[str]) -> Dict[str, Any]:
    command = ["bash", str(ROOT / "scripts" / script)] + args
    try:
        result = subprocess.run(
            command,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("%s query failed: %s" % (script, exc))
    if result.returncode != 0:
        detail = result.stderr.strip().splitlines()
        fail("%s query failed%s" % (script, ": " + detail[-1] if detail else ""))
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        fail("%s returned invalid JSON" % script)
    if not isinstance(value, dict) or value.get("schema") != 1:
        fail("%s returned an unsupported contract" % script)
    return value


def read_list_surface(script: str, args: List[str]) -> List[Dict[str, Any]]:
    command = ["bash", str(ROOT / "scripts" / script)] + args
    try:
        result = subprocess.run(
            command,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        fail("%s query failed: %s" % (script, exc))
    if result.returncode != 0:
        detail = result.stderr.strip().splitlines()
        fail("%s query failed%s" % (script, ": " + detail[-1] if detail else ""))
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        fail("%s returned invalid JSON" % script)
    if not isinstance(value, list) or any(not isinstance(row, dict) for row in value):
        fail("%s returned an unsupported contract" % script)
    return value


def read_jsonl_tail(path: Path, limit: int) -> List[Dict[str, Any]]:
    try:
        with path.open(encoding="utf-8") as handle:
            lines = handle.readlines()
    except OSError:
        return []
    rows = []
    for line in lines[-limit:]:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(row, dict):
            rows.append(row)
    return rows


def read_observations(repo: str, limit: int) -> Dict[str, Any]:
    """The pending observation decisions, made measurable in one place.

    Advisory projections only: turn-guard intervention pairing, fail-ledger
    hook-row retirement, and usage-family exposure. No thresholds and no
    tuning — the keep/kill judgment stays with the operator. Guard rows
    written before instrumentation carry no observation key and are counted
    as uninstrumented rather than silently folded in.
    """
    oms = Path(repo) / ".oms"

    guard = {
        "instrumented_rows": 0,
        "uninstrumented_rows": 0,
        "eligible_turns": 0,
        "blocked_turns": 0,
        "corrected_after_block": 0,
        "by_agent": {},
    }
    eligible_keys = set()
    blocked_keys = set()
    corrected_keys = set()
    for row in read_jsonl_tail(oms / "hooks" / "events.jsonl", limit):
        if row.get("action") != "turn_guard":
            continue
        obs = row.get("turn_obs")
        if not isinstance(obs, str) or not obs:
            guard["uninstrumented_rows"] += 1
            continue
        guard["instrumented_rows"] += 1
        agent = str(row.get("agent") or "?")
        guard["by_agent"][agent] = guard["by_agent"].get(agent, 0) + 1
        status = row.get("status")
        if row.get("eligible") is True:
            eligible_keys.add(obs)
        if status == "block_unverified":
            blocked_keys.add(obs)
        elif status == "allow_verified" and obs in blocked_keys:
            corrected_keys.add(obs)
    guard["eligible_turns"] = len(eligible_keys)
    guard["blocked_turns"] = len(blocked_keys)
    guard["corrected_after_block"] = len(corrected_keys)

    ledger = read_surface("fail-ledger.sh", ["--repo", repo, "list", "--json"])
    hook_rows = [
        row
        for row in (ledger.get("failures") or [])
        if isinstance(row, dict) and row.get("kind") == "hook"
    ]
    failures = {
        "hook_rows": len(hook_rows),
        "hook_open": sum(
            1 for row in hook_rows if not row.get("resolved") and not row.get("expired")
        ),
        "hook_expired": sum(1 for row in hook_rows if row.get("expired")),
        "hook_recurred": sum(
            1 for row in hook_rows if isinstance(row.get("count"), int) and row["count"] > 1
        ),
    }

    usage_path = oms / "usage.jsonl"
    usage_rows = read_jsonl_tail(usage_path, limit)
    families: Dict[str, int] = {}
    for row in usage_rows:
        family = row.get("family")
        if not isinstance(family, str) or not family:
            continue
        count = row.get("count")
        families[family] = families.get(family, 0) + (
            count if isinstance(count, int) and count > 0 else 1
        )
    usage = {
        "file_present": usage_path.is_file(),
        "rows": len(usage_rows),
        "families": families,
    }

    return {"guard": guard, "failures": failures, "usage": usage}


def count_priorities(items: List[Dict[str, Any]]) -> Dict[str, int]:
    counts = {"P1": 0, "P2": 0, "P3": 0}
    for item in items:
        priority = item.get("priority")
        if priority in counts:
            counts[priority] += 1
    return counts


def build_report(repo: str, limit: int) -> Dict[str, Any]:
    state = read_surface("state.sh", ["--repo", repo, "--json"])
    inbox = read_surface("inbox.sh", ["--repo", repo, "--json"])
    telemetry = read_surface(
        "artifact-index.sh",
        ["--repo", repo, "--json", "telemetry", str(limit)],
    )
    attempts = read_list_surface(
        "agent-events.sh",
        ["--repo", repo, "list", "--active", "--limit", str(limit), "--json"],
    )
    approvals = read_list_surface(
        "approval-inbox.sh", ["--repo", repo, "list", "--pending", "--json"]
    )

    plan = state.get("plan") if isinstance(state.get("plan"), dict) else {}
    by_state = plan.get("by_state") if isinstance(plan.get("by_state"), dict) else {}
    delegations = (
        state.get("delegations") if isinstance(state.get("delegations"), list) else []
    )
    live = sum(1 for row in delegations if isinstance(row, dict) and row.get("live"))
    orphan = sum(
        1 for row in delegations if isinstance(row, dict) and not row.get("live")
    )
    items = inbox.get("items") if isinstance(inbox.get("items"), list) else []
    items = [row for row in items if isinstance(row, dict)]
    attention = count_priorities(items)
    runs = state.get("runs") if isinstance(state.get("runs"), dict) else {}

    review = int(by_state.get("review", 0) or 0)
    stale_review = len(plan.get("stale_review", []) or [])
    outstanding_landings = len(
        (state.get("landings") or {}).get("outstanding", [])
        if isinstance(state.get("landings"), dict)
        else []
    )
    guard = (
        state.get("change_guard")
        if isinstance(state.get("change_guard"), dict)
        else {}
    )
    approval_reasons = []
    if review:
        approval_reasons.append("plan-review")
    if stale_review:
        approval_reasons.append("stale-review")
    if outstanding_landings:
        approval_reasons.append("interrupted-landing")
    if guard.get("active") and guard.get("stale"):
        approval_reasons.append("stale-change-guard")
    requested_approvals = sum(1 for row in approvals if row.get("state") == "requested")
    if requested_approvals:
        approval_reasons.append("approval-request")

    actionable = len(plan.get("actionable", []) or [])
    if attention["P1"]:
        phase = "attention"
    elif requested_approvals:
        phase = "waiting-approval"
    elif attempts:
        phase = "running"
    elif live:
        phase = "running"
    elif review:
        phase = "review"
    elif actionable:
        phase = "ready"
    elif (state.get("task") or {}).get("present"):
        phase = "active"
    else:
        phase = "idle"

    lifecycle = {
        "phase": phase,
        "live_delegations": live,
        "orphan_delegations": orphan,
        "claimed_tasks": int(by_state.get("claimed", 0) or 0),
        "review_tasks": review,
        "actionable_tasks": actionable,
        "open_runs": len(runs.get("open", []) or []),
        "attention": attention,
        "active_attempts": len(attempts),
        "attempts_by_state": {
            name: sum(1 for row in attempts if row.get("state") == name)
            for name in sorted(
                {
                    str(row.get("state"))
                    for row in attempts
                    if isinstance(row.get("state"), str)
                }
            )
        },
        "attempts": attempts,
    }
    approval = {
        "required": bool(approval_reasons),
        "reasons": approval_reasons,
        "requested": requested_approvals,
        "pending": len(approvals),
        "items": approvals,
    }
    return {
        "schema": 1,
        "action": "ops-cockpit",
        "state": state,
        "inbox": inbox,
        "telemetry": telemetry,
        "lifecycle": lifecycle,
        "approval": approval,
        "observations": read_observations(repo, limit),
    }


def emit_text(report: Dict[str, Any]) -> None:
    lifecycle = report["lifecycle"]
    attention = lifecycle["attention"]
    approval = "yes" if report["approval"]["required"] else "no"
    print(
        "ops cockpit: phase=%s approval=%s attention=P1:%d/P2:%d/P3:%d"
        % (
            lifecycle["phase"],
            approval,
            attention["P1"],
            attention["P2"],
            attention["P3"],
        )
    )
    print(
        "work: attempts=%d live=%d orphan=%d claimed=%d review=%d ready=%d open-runs=%d"
        % (
            lifecycle["active_attempts"],
            lifecycle["live_delegations"],
            lifecycle["orphan_delegations"],
            lifecycle["claimed_tasks"],
            lifecycle["review_tasks"],
            lifecycle["actionable_tasks"],
            lifecycle["open_runs"],
        )
    )
    telemetry = report["telemetry"]
    operations = (telemetry.get("operations") or {}).get("eligible", 0)
    usage = telemetry.get("usage") or {}
    tokens = (usage.get("provider_reported_tokens") or {}).get("total", 0)
    wall = (usage.get("wall_seconds") or {}).get("total", 0)
    print("usage: operations=%s tokens=%s wall-seconds=%s" % (operations, tokens, wall))
    observations = report["observations"]
    obs_guard = observations["guard"]
    print(
        "observations: guard-turns=%d blocked=%d corrected=%d hook-fails=%d/%d usage-rows=%d"
        % (
            obs_guard["eligible_turns"],
            obs_guard["blocked_turns"],
            obs_guard["corrected_after_block"],
            observations["failures"]["hook_open"],
            observations["failures"]["hook_rows"],
            observations["usage"]["rows"],
        )
    )
    for item in report["inbox"].get("items", [])[:3]:
        print("%s %s: %s" % (item.get("priority"), item.get("code"), item.get("summary")))
        print("  next: %s" % item.get("command"))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Compose repo state, inbox, telemetry, lifecycle, and approval signals."
    )
    parser.add_argument("--repo", default=".", help="repository to inspect")
    parser.add_argument("--limit", type=positive, default=1000, help="maximum rows read per surface")
    parser.add_argument("--json", action="store_true", help="emit the full operational metadata report")
    args = parser.parse_args()

    report = build_report(args.repo, args.limit)
    if args.json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    else:
        emit_text(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
