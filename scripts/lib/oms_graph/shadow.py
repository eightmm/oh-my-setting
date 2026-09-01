"""Evaluator-vs-control-plane comparison ledger (W3).

The evaluator takes no authority from `goal-drive`/`autopilot` in this round.
One shadow call evaluates the bundled spec against current facts, asks the
control plane for its own canonical next action, and records whether the two
agreed. A disagreement is evidence for the next round, never an action — the
same discipline `autopilot shadow` already follows.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, Mapping

from oms_runtime.common import append_jsonl, bounded_line, install_root, run_json, utc_now

from .facts import collect_facts
from .route import evaluate, state_from_outcomes
from .spec import load_spec, spec_digest

CONTROL_PLANE_TIMEOUT = 60

# The deterministic transitions `oms runtime next` shares with inbox and state,
# mapped onto the node the graph would run for the same reality.
ACTION_ROUTES = {
    "execute_ready_task": "implement",
    "review_or_land_patch": "land",
    "finish_landing": "land",
    "record_verified_completion": "done",
    "inspect_completed_plan_retirement": "done",
    "resolve_blocker": "blocked",
    "inspect_plan_contract": "blocked",
    "orient": "inspect",
}


def control_plane_action(repo: Path) -> str:
    """The control plane's own first next action, or '' when it names none."""
    payload = run_json(["bash", str(install_root() / "scripts" / "runtime.sh"), "--repo", str(repo), "next"],
                       cwd=repo, timeout=CONTROL_PLANE_TIMEOUT)
    actions = (payload or {}).get("actions")
    if not isinstance(actions, list) or not actions or not isinstance(actions[0], Mapping):
        return ""
    return str(actions[0].get("id", "") or "")


def shadow(repo: Path, *, spec_name: str = "goal-drive") -> Dict[str, Any]:
    repo = Path(repo).resolve()
    spec = load_spec(spec_name)
    facts = collect_facts(repo)
    route = evaluate(spec, state_from_outcomes(spec, {}), facts)
    action = control_plane_action(repo)
    mapped = ACTION_ROUTES.get(action, "")
    status = str(route.get("status", ""))
    primary = str(route.get("primary") or "")
    # `blocked` is a status, not a node: the control plane naming a blocker
    # agrees with any route that refuses to advance for the same reason.
    agree = bool(mapped) and (mapped == primary or (mapped == "blocked" and status in ("blocked", "exhausted")))
    row = {
        "schema": 1,
        "kind": "graph-route-shadow",
        "ts": utc_now(),
        "spec_id": str(spec.get("id", "")),
        "spec_digest": spec_digest(spec),
        "route": {"status": status, "primary": primary, "reason": bounded_line(route.get("reason", ""), 300)},
        "control_plane": {"action": action, "mapped": mapped},
        "agree": agree,
        "reason": bounded_line("control plane %s maps to %s; route %s %s"
                               % (action or "-", mapped or "-", status or "-", primary or "-"), 300),
    }
    append_jsonl(repo / ".oms" / "graph" / "shadow.jsonl", row)
    return row
