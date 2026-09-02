"""Read-only projections of Git, plan, and receipt state."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Sequence

from oms_runtime.common import install_root, read_json, run_json, run_output
from oms_runtime.evidence import artifact_rows

from .errors import GraphError


def collect_facts(repo: Path, *, include: Sequence[str] = ("git", "plan", "receipts")) -> Dict[str, Any]:
    """Flat fact dict with the keys listed in docs/GRAPH-ENGINEERING.md."""
    repo = Path(repo).resolve()
    requested = list(include)
    unknown = sorted(set(requested) - {"git", "plan", "receipts"})
    if unknown:
        raise GraphError("unknown fact collector: %s" % ", ".join(unknown))
    result: Dict[str, Any] = {}
    if "git" in requested:
        result.update(git_facts(repo))
    if "plan" in requested:
        result.update(plan_facts(repo))
    if "receipts" in requested:
        head = str(result.get("git.head", ""))
        if not head:
            head = run_output(["git", "-C", str(repo), "rev-parse", "HEAD"], cwd=repo)
        result.update(receipt_facts(repo, head=head))
    return dict(sorted(result.items()))


def git_facts(repo: Path) -> Dict[str, Any]:
    repo = Path(repo).resolve()
    return {
        "git.head": run_output(["git", "-C", str(repo), "rev-parse", "HEAD"], cwd=repo),
        "git.branch": run_output(["git", "-C", str(repo), "symbolic-ref", "--short", "HEAD"], cwd=repo),
        "git.dirty": bool(run_output(["git", "-C", str(repo), "status", "--porcelain"], cwd=repo)),
    }


def plan_facts(repo: Path) -> Dict[str, Any]:
    """agent-plan status --json plus show --id per task; never writes."""
    repo = Path(repo).resolve()
    script = install_root() / "scripts" / "agent-plan.sh"
    status = run_json(["bash", str(script), "--repo", str(repo), "status", "--json"], cwd=repo)
    if not isinstance(status, Mapping):
        raise GraphError("canonical plan status projection is unavailable")
    result: Dict[str, Any] = {}
    for key in ("present", "all_done", "has_unfinished"):
        value = status.get(key)
        if not isinstance(value, bool):
            raise GraphError("canonical plan status has invalid %s" % key)
        result["plan.%s" % key] = value
    actionable = status.get("actionable", [])
    if not isinstance(actionable, list) or any(not isinstance(item, str) for item in actionable):
        raise GraphError("canonical plan status has invalid actionable tasks")
    result["plan.actionable"] = list(actionable)
    contract = status.get("contract", {})
    if not isinstance(contract, Mapping) or not isinstance(contract.get("satisfied"), bool):
        raise GraphError("canonical plan status has invalid contract")
    result["plan.contract.satisfied"] = contract["satisfied"]

    # One `list --json` is the same read view `show --id` computes, for every
    # task in one agent-plan process; per-task `show` made a route or shadow
    # cost one interpreter start per plan task.
    if not status.get("present"):
        return dict(sorted(result.items()))
    listing = run_json(["bash", str(script), "--repo", str(repo), "list", "--json"], cwd=repo)
    if not isinstance(listing, Mapping) or not isinstance(listing.get("tasks"), list):
        raise GraphError("canonical plan task projection is unavailable")
    views: Dict[str, Mapping] = {}
    for view in listing["tasks"]:
        if not isinstance(view, Mapping) or not isinstance(view.get("id"), str) or not view["id"]:
            raise GraphError("canonical plan task projection contains an invalid task")
        views[view["id"]] = view
    for task_id in sorted(views):
        view = views[task_id]
        prefix = "plan.task.%s." % task_id
        result[prefix + "state"] = str(view.get("state", ""))
        result[prefix + "patch_present"] = bool(view.get("patch"))
        result[prefix + "artifact_present"] = bool(view.get("artifact"))
        result[prefix + "lease_present"] = bool(view.get("lease_id"))
        result[prefix + "claim_expired"] = bool(view.get("claim_expired", False))
        repair_count = view.get("repair_count", 0)
        result[prefix + "repair_count"] = repair_count if isinstance(repair_count, int) and not isinstance(repair_count, bool) else 0
        result[prefix + "reason"] = str(view.get("reason", ""))
    return result


def receipt_facts(repo: Path, *, head: str = "") -> Dict[str, Any]:
    """From oms_runtime.evidence.artifact_rows and .oms/plan/progress.jsonl."""
    result: Dict[str, Any] = {}
    latest_admit: Dict[str, str] = {}
    land_tasks: Dict[str, bool] = {}
    latest_acceptance: Mapping[str, Any] = {}
    for row in artifact_rows(Path(repo).resolve()):
        kind = str(row.get("kind", ""))
        if kind == "patch-admit" and row.get("task_id"):
            task_id = str(row.get("task_id"))
            latest_admit[task_id] = "verified" if _exit_zero(row.get("exit")) else "failed"
        elif kind == "patch-land" and row.get("task_id"):
            task_id = str(row.get("task_id"))
            land_tasks[task_id] = land_tasks.get(task_id, False) or _exit_zero(row.get("exit"))
        elif kind == "acceptance" and str(row.get("status", "")) in ("pass", "fail", "error"):
            latest_acceptance = row
    for task_id, status in sorted(latest_admit.items()):
        result["receipt.admit.%s.latest" % task_id] = status
    for task_id, present in sorted(land_tasks.items()):
        result["receipt.land.%s.present" % task_id] = present
    if latest_acceptance:
        status = str(latest_acceptance.get("status"))
        base_sha = str(latest_acceptance.get("base_sha", ""))
        result["receipt.acceptance.latest"] = status
        result["receipt.acceptance.base_sha"] = base_sha
        result["receipt.acceptance.fresh"] = bool(head and base_sha == head)
    return result


def _exit_zero(value: Any) -> bool:
    return not isinstance(value, bool) and (value == 0 or value == "0")
