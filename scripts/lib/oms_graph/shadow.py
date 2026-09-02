"""Evaluator-vs-control-plane comparison ledger (W3).

The evaluator takes no authority from `goal-drive`/`autopilot` in this round.
One shadow call reconstructs where the bundled graph would stand against
current reality, asks the control plane for its own canonical next action, and
records whether the two agreed. A disagreement is evidence for the next round,
never an action — the same discipline `autopilot shadow` already follows.

Reconstruction, not replay: no run exists in a repository that never used
`exec run`, and an empty run's primary is always the entry node, which would
compare nothing. Starting from that empty state, a primary whose proof already
holds under the facts has in reality been done — it is settled `completed`
(agent nodes bound to the task reality names) and the route re-evaluated —
until the primary is a node reality cannot confirm. That node is the frontier:
the work reality still owes, and the point of comparison.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping

from oms_runtime.common import append_jsonl, bounded_line, install_root, run_json, utc_now

from .binding import augment_binding_facts
from .facts import collect_facts
from .route import effective_outcome, evaluate, state_from_outcomes
from .spec import load_spec, normalize_spec, spec_digest

CONTROL_PLANE_TIMEOUT = 60
# Every settle completes one node, so a reconstruction is bounded by the graph
# plus its repeat budget; this cap only guards a pathological spec.
RECONSTRUCT_LIMIT = 64
# Plan states of a task the harness is already working on: such a task, not
# the plan's FIFO `next`, is what a `next` selector would have bound.
IN_FLIGHT_STATES = ("landing", "review", "running", "claimed")

# The deterministic transitions `oms runtime next` shares with inbox and state,
# mapped onto the node the graph would run for the same reality.
ACTION_ROUTES = {
    "execute_ready_task": "implement",
    "review_or_land_patch": "land",
    "finish_landing": "land",
    "verify_active_task": "acceptance",
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


def reality_task(facts: Mapping[str, Any]) -> str:
    """The task a `next` selector would bind against current reality.

    A task already in flight (claimed, running, in review, landing) is the one
    the harness is working on; otherwise the plan's first actionable task."""
    states: Dict[str, str] = {}
    for key, value in facts.items():
        if key.startswith("plan.task.") and key.endswith(".state"):
            states[key[len("plan.task."):-len(".state")]] = str(value)
    for wanted in IN_FLIGHT_STATES:
        matching = sorted(task_id for task_id, state in states.items() if state == wanted)
        if matching:
            return matching[0]
    actionable = facts.get("plan.actionable")
    if isinstance(actionable, list) and actionable and isinstance(actionable[0], str):
        return actionable[0]
    return ""


def reconstruct(spec: Mapping[str, Any], facts: Mapping[str, Any]) -> Dict[str, Any]:
    """Pure: the route at the frontier reality implies, plus how it was reached.

    Settling rules, in order: a primary whose proof holds is `completed`; an
    effect-free tool (a check such as `acceptance`) whose proof does not hold
    is assumed `failed`, so the route reaches the effectful work the check
    guards; any other unproven node — and a node without a proof — is the
    frontier. A check assumed failed whose failure path finds no task to bind
    is itself the frontier: reality owes the check, not work."""
    graph = normalize_spec(spec)
    nodes = graph.get("nodes", {})
    outcomes: Dict[str, str] = {}
    bindings: Dict[str, Dict[str, Any]] = {}
    completed: List[str] = []
    assumed_failed: List[str] = []
    route: Dict[str, Any] = {}
    stop = ""
    for _ in range(RECONSTRUCT_LIMIT):
        state = state_from_outcomes(spec, outcomes, bindings=bindings)
        route = evaluate(spec, state, facts)
        primary = str(route.get("primary") or "")
        if route.get("status") != "actionable" or not primary or primary in outcomes:
            stop = "route" if route.get("status") != "actionable" else "repeat"
            break
        node = nodes.get(primary, {})
        bind = str(node.get("bind_task") or "")
        if bind and bind not in bindings:
            task_id = reality_task(facts)
            if not task_id:
                stop = "unbound"
                break
            bindings[bind] = {"task_id": task_id, "node": primary, "attempt": 1}
            state = state_from_outcomes(spec, outcomes, bindings=bindings)
        if not node.get("proof"):
            stop = "undecidable"
            break
        outcome, absent = effective_outcome(node, "completed", augment_binding_facts(graph, state, facts))
        if not absent and outcome == "completed":
            outcomes[primary] = "completed"
            completed.append(primary)
            continue
        if node.get("kind") == "tool" and str(node.get("effect", "") or "") in ("", "read", "none"):
            outcomes[primary] = "failed"
            assumed_failed.append(primary)
            continue
        stop = "unproven"
        break
    else:
        stop = "limit"
    primary = str(route.get("primary") or "")
    if stop == "unbound" and assumed_failed:
        primary, stop = assumed_failed[-1], "check"
    successors = sorted({str(edge.get("to", "")) for edge in graph.get("edges", []) if str(edge.get("from", "")) == primary and edge.get("to")})
    return {
        "route": route,
        "frontier": primary,
        "frontier_kind": str(nodes.get(primary, {}).get("kind", "")) if primary else "",
        "frontier_effect": str(nodes.get(primary, {}).get("effect", "") or "") if primary else "",
        "successors": successors,
        "completed": completed,
        "assumed_failed": assumed_failed,
        "bindings": {name: entry["task_id"] for name, entry in sorted(bindings.items())},
        "stop": stop,
    }


def compare(reconstruction: Mapping[str, Any], action: str) -> Dict[str, Any]:
    """Pure: does the control plane's action name what the graph would do next?

    Exact agreement is the frontier itself. A frontier that is an effect-free
    tool (a check such as `acceptance`) agrees with an action naming one of its
    immediate successors: both sides then name the same next *effectful* step,
    the check being the graph's way of reading the state the control plane read
    directly. `blocked` is a status, not a node: the control plane naming a
    blocker agrees with any route that refuses to advance."""
    mapped = ACTION_ROUTES.get(action, "")
    route = reconstruction.get("route", {})
    status = str(route.get("status", ""))
    frontier = str(reconstruction.get("frontier", ""))
    basis = ""
    if mapped and mapped == frontier:
        basis = "frontier"
    elif mapped == "blocked" and status in ("blocked", "exhausted"):
        basis = "blocked"
    elif (mapped and reconstruction.get("frontier_kind") == "tool"
          and reconstruction.get("frontier_effect") in ("", "read", "none")
          and mapped in reconstruction.get("successors", [])):
        basis = "successor"
    return {"action": action, "mapped": mapped, "agree": bool(basis), "basis": basis}


def shadow(repo: Path, *, spec_name: str = "goal-drive") -> Dict[str, Any]:
    repo = Path(repo).resolve()
    spec = load_spec(spec_name)
    facts = collect_facts(repo)
    reconstruction = reconstruct(spec, facts)
    verdict = compare(reconstruction, control_plane_action(repo))
    route = reconstruction["route"]
    status = str(route.get("status", ""))
    primary = str(reconstruction.get("frontier") or "")
    row = {
        "schema": 1,
        "kind": "graph-route-shadow",
        "ts": utc_now(),
        "spec_id": str(spec.get("id", "")),
        "spec_digest": spec_digest(spec),
        "route": {"status": status, "primary": primary, "reason": bounded_line(route.get("reason", ""), 300)},
        "reconstructed": {"completed": list(reconstruction["completed"]), "assumed_failed": list(reconstruction["assumed_failed"]),
                          "bindings": dict(reconstruction["bindings"]), "successors": list(reconstruction["successors"]),
                          "stop": reconstruction["stop"]},
        "control_plane": {"action": verdict["action"], "mapped": verdict["mapped"]},
        "agree": verdict["agree"],
        "basis": verdict["basis"],
        "reason": bounded_line("control plane %s maps to %s; route %s %s after %s"
                               % (verdict["action"] or "-", verdict["mapped"] or "-", status or "-", primary or "-",
                                  ",".join(reconstruction["completed"]) or "nothing settled"), 300),
    }
    append_jsonl(repo / ".oms" / "graph" / "shadow.jsonl", row)
    return row
