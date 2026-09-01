"""`exec run` step loop: tool nodes, agent nodes via adapters, gates, caching, resume (W3).

Nothing here advances plan state on its own. Agent work goes through
`adapters.plan.execute`, whose only front doors are `plan-run` and the
read-only `agent-plan` verbs; a tool node runs its own declared command; and a
gate outcome exists only once a parent records it with `decide`. Every
transition is an append-only event and `projection.json` is a cache of the
fold, never an input, so a killed run is recoverable from `events.jsonl`
alone.

A claimed completion is never the outcome: `route.effective_outcome` re-checks
the node's `proof` predicates against facts collected *after* the work ran, and
a claim without its evidence is recorded as `unverified`.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import (
    atomic_write_bytes, atomic_write_json, bounded_line, canonical_json,
    install_root, read_json, sensitive_text, sha256_bytes, utc_now,
)

from . import EXEC_SCHEMA, OUTCOMES
from . import events as events_module
from . import predicates
from . import route as route_module
from . import scheduler
from . import spec as spec_module
from .adapters import plan as plan_adapter
from .errors import GraphError
from .facts import collect_facts
from .validate import validate_spec

RUNNER_ACTOR = {"kind": "runner", "name": "oms-graph"}
RESUME_ACTOR = {"kind": "runner", "name": "resume"}
PARENT_ACTOR = {"kind": "parent", "name": "decide"}

# The Work Journal observer is a shell function that takes the harness lock
# before entering its store; the module it wraps is explicitly not a front
# door. This is the only script this module names, and it never affects a run.
JOURNAL_OBSERVER = "work-journal.sh"
JOURNAL_TIMEOUT = 20
# The journal identifies an event by (source type, source id), so a run holds
# exactly one row: its finish. A per-gate row under the same id would hide the
# run's own outcome rather than add to it.
JOURNAL_EVENT_TYPE = "phase_outcome"

AGENT_TIMEOUT = 2700
TOOL_TIMEOUT = 600
TAIL_LIMIT = 4000
DETAIL_LIMIT = 400

# plan-run and patch-land verdicts that are transport facts rather than a
# worker's judgement: exit 3 is "no actionable task", exit 75 is "another
# landing holds the lock". Both are recorded verbatim so a retry is a
# deliberate route decision, not a re-interpretation.
TRANSPORT_EXITS = {3: ("blocked", "no-actionable-task"), 75: ("partial", "landing-lock-held")}


# --------------------------------------------------------------------------
# pure helpers
# --------------------------------------------------------------------------

def alias_facts(facts: Mapping[str, Any], rows: Sequence[Mapping[str, Any]], spec: Mapping[str, Any]) -> Dict[str, Any]:
    """Mirror the task a `plan_task: next` node resolved onto the `next` alias.

    The bundled `goal-drive` spec names the plan's next actionable task rather
    than a fixed id, so its proof predicates are written against
    `plan.task.next.*` / `receipt.*.next.*`. Only the run's own events know
    which task `plan-run --next` actually selected, so the alias is derived
    from the recorded `task_id` (latest row wins) and never guessed.
    """
    nodes = spec.get("nodes", {})
    if not isinstance(nodes, Mapping):
        return dict(facts)
    next_nodes = set()
    for node_id, node in nodes.items():
        if isinstance(node, Mapping) and str(node.get("plan_task", "") or "") == plan_adapter.NEXT_TASK:
            next_nodes.add(str(node_id))
    if not next_nodes:
        return dict(facts)
    task_id = ""
    for row in rows:
        if not isinstance(row, Mapping) or row.get("event") != "node_outcome":
            continue
        if str(row.get("node", "")) not in next_nodes:
            continue
        candidate = str(row.get("task_id", "") or "")
        if candidate:
            task_id = candidate
    if not task_id or task_id == plan_adapter.NEXT_TASK:
        return dict(facts)
    result = dict(facts)
    plan_prefix = "plan.task.%s." % task_id
    for key, value in facts.items():
        if key.startswith(plan_prefix):
            result["plan.task.next.%s" % key[len(plan_prefix):]] = value
            continue
        parts = key.split(".")
        if len(parts) >= 4 and parts[0] == "receipt" and parts[2] == task_id:
            result[".".join(["receipt", parts[1], "next"] + parts[3:])] = value
    return result


def proof_facts(node_spec: Mapping[str, Any], facts: Mapping[str, Any]) -> Dict[str, Any]:
    """The fact slice a node's own proof predicates name, for the event row."""
    subset: Dict[str, Any] = {}
    for predicate in node_spec.get("proof", []) or []:
        try:
            key = predicates.parse(str(predicate))[0]
        except GraphError:
            continue
        if key in facts:
            subset[key] = facts[key]
    return subset


def agent_outcome(result: Mapping[str, Any]) -> Dict[str, Any]:
    """Adapter result -> the fields one `node_outcome` row records.

    The adapter owns reading the plan; this owns what the event says. The two
    transport verdicts and a timeout are asserted from the exit status here so
    a mis-mapped adapter result cannot record a landing lock as a failure.
    """
    claimed = str(result.get("claimed_outcome", "") or "")
    outcome = str(result.get("outcome", "") or "")
    reason = str(result.get("reason", "") or "")
    exit_code = result.get("exit")
    transport = TRANSPORT_EXITS.get(exit_code) if isinstance(exit_code, int) and not isinstance(exit_code, bool) else None
    if reason == "timeout":
        claimed = outcome = "unverified"
    elif transport is not None:
        claimed = outcome = transport[0]
        reason = reason or transport[1]
    if outcome not in OUTCOMES:
        claimed = outcome = "unverified"
        reason = reason or "adapter-returned-no-outcome"
    missing = [str(item) for item in (result.get("proof_missing") or [])]
    detail = reason
    if missing:
        detail = "%s proof-missing=%s" % (detail, ",".join(missing)) if detail else "proof-missing=%s" % ",".join(missing)
    return {
        "claimed_outcome": claimed or outcome,
        "outcome": outcome,
        "detail": bounded_line(detail, DETAIL_LIMIT),
        "facts": dict(result.get("facts") or {}),
        "stdout_tail": str(result.get("stdout_tail") or ""),
        "stderr_tail": str(result.get("stderr_tail") or ""),
    }


def _cacheable(node: Mapping[str, Any]) -> bool:
    """Read-only tool nodes only. Write, land, gate, and agent work never caches."""
    return (str(node.get("kind", "")) == "tool" and str(node.get("effect", "read")) == "read"
            and bool(node.get("cacheable", False)))


def _cache_key(node: Mapping[str, Any], state: Mapping[str, Any], facts: Mapping[str, Any], node_id: str) -> str:
    node_states = state.get("nodes", {}) if isinstance(state.get("nodes", {}), Mapping) else {}
    upstream = {}
    for other, value in node_states.items():
        if other == node_id or not isinstance(value, Mapping) or value.get("status") != "finished":
            continue
        upstream[str(other)] = value.get("outcome")
    payload = {"schema": EXEC_SCHEMA, "node": dict(node), "head": facts.get("git.head"),
               "upstream": dict(sorted(upstream.items()))}
    return sha256_bytes(canonical_json(payload))


def _artifact_name(node_id: str, attempt: int) -> str:
    safe = "".join(char if (char.isalnum() or char in "._-") else "_" for char in str(node_id))
    return "%s-%d.txt" % (safe or "node", attempt)


# --------------------------------------------------------------------------
# run state
# --------------------------------------------------------------------------

def _read_state(repo: Path, run_id: str, spec: Mapping[str, Any]) -> Tuple[List[Dict[str, Any]], Dict[str, Any], Dict[str, Any]]:
    rows = events_module.read_events(repo, run_id)
    state = events_module.project(rows, spec)
    return rows, state, alias_facts(collect_facts(repo), rows, spec)


def _task_scopes(repo: Path, spec: Mapping[str, Any]) -> Dict[str, Dict[str, List[str]]]:
    """Declared write scope per agent node's plan task; a missing task is left out."""
    scopes: Dict[str, Dict[str, List[str]]] = {}
    for node in spec.get("nodes", {}).values():
        if not isinstance(node, Mapping) or node.get("kind") != "agent":
            continue
        task_id = str(node.get("plan_task", "") or "")
        if not task_id or task_id == plan_adapter.NEXT_TASK or task_id in scopes:
            continue
        try:
            scopes[task_id] = plan_adapter.scope_of(plan_adapter.task_view(repo, task_id))
        except GraphError:
            # The scheduler reports `unknown-scope` rather than guessing.
            continue
    return scopes


def _write_artifact(repo: Path, run_id: str, node_id: str, attempt: int, body: str) -> None:
    """The node's own output tail, line structure kept, secret-shaped text refused."""
    tail = str(body or "")[-TAIL_LIMIT:]
    payload = b"tail withheld: secret-shaped output\n" if sensitive_text(tail) else tail.encode("utf-8", "replace")
    path = events_module.run_dir(repo, run_id) / "artifacts" / _artifact_name(node_id, attempt)
    atomic_write_bytes(path, payload)


def _append_outcome(repo: Path, run_id: str, node_id: str, attempt: int, record: Mapping[str, Any],
                    *, actor: Mapping[str, Any]) -> Dict[str, Any]:
    """Append one node_outcome, dropping a detail the durable-writers contract refuses.

    Free text from a worker can trip the secret-shaped scrubber; the run must
    not die because a tail looked like a credential, and the full tail is
    already in the run's artifacts directory.
    """
    fields = {
        "node": node_id,
        "attempt": attempt,
        "idempotency_key": "outcome:%s:%d" % (node_id, attempt),
        "claimed_outcome": record.get("claimed_outcome"),
        "outcome": record.get("outcome"),
        "facts": dict(record.get("facts") or {}),
        "cached": bool(record.get("cached", False)),
        "actor": dict(actor),
        "detail": bounded_line(record.get("detail", ""), DETAIL_LIMIT),
    }
    task_id = str(record.get("task_id", "") or "")
    if task_id:
        fields["task_id"] = task_id
    try:
        return events_module.append_event(repo, run_id, "node_outcome", **fields)
    except GraphError:
        fields["detail"] = "detail-withheld"
        return events_module.append_event(repo, run_id, "node_outcome", **fields)


# --------------------------------------------------------------------------
# node execution
# --------------------------------------------------------------------------

def _run_agent(repo: Path, node: Mapping[str, Any], options: Mapping[str, Any]) -> Dict[str, Any]:
    result = plan_adapter.execute(
        repo, node,
        provider=str(node.get("provider") or options.get("worker") or ""),
        model=str(options.get("model") or ""),
        reasoning_effort=str(options.get("reasoning_effort") or ""),
        timeout=int(node.get("timeout", AGENT_TIMEOUT) or AGENT_TIMEOUT),
    )
    record = agent_outcome(result)
    task = result.get("task")
    if isinstance(task, Mapping) and task.get("id"):
        record["task_id"] = str(task.get("id"))
    return record


def _tool_env(run_id: str, node_id: str, attempt: int, goal: str) -> Dict[str, str]:
    env = dict(os.environ)
    env.update({
        "OMS_GRAPH_RUN_ID": run_id,
        "OMS_GRAPH_NODE": node_id,
        "OMS_GRAPH_ATTEMPT": str(attempt),
        "OMS_GRAPH_GOAL": str(goal or ""),
    })
    return env


def _run_tool(repo: Path, run_id: str, spec: Mapping[str, Any], node_id: str, node: Mapping[str, Any],
              attempt: int, state: Mapping[str, Any], facts: Mapping[str, Any],
              options: Mapping[str, Any]) -> Dict[str, Any]:
    cache_key = _cache_key(node, state, facts, node_id) if _cacheable(node) else ""
    if cache_key:
        hit = _cache_read(repo, cache_key)
        if hit is not None:
            hit["facts"] = proof_facts(node, facts)
            return hit
    command = str(node.get("command", "") or "")
    if not command:
        raise GraphError("tool node %s has no command" % bounded_line(node_id, 60))
    try:
        completed = subprocess.run(
            ["bash", "-c", command], cwd=str(repo),
            env=_tool_env(run_id, node_id, attempt, str(options.get("goal", ""))),
            capture_output=True, text=True,
            timeout=int(node.get("timeout", TOOL_TIMEOUT) or TOOL_TIMEOUT),
        )
    except subprocess.TimeoutExpired as exc:
        return {"claimed_outcome": "unverified", "outcome": "unverified", "detail": "timeout",
                "facts": proof_facts(node, facts),
                "stdout_tail": _decode(exc.stdout), "stderr_tail": _decode(exc.stderr)}
    except OSError as exc:
        raise GraphError("tool node %s could not run: %s" % (bounded_line(node_id, 60), bounded_line(exc, 200)))
    claimed = "completed" if completed.returncode == 0 else "failed"
    facts_after = _read_state(repo, run_id, spec)[2]
    outcome, absent = route_module.effective_outcome(node, claimed, facts_after)
    detail = "exit=%d" % completed.returncode
    if absent:
        detail = "%s proof-missing=%s" % (detail, ",".join(absent))
    record = {"claimed_outcome": claimed, "outcome": outcome, "detail": detail,
              "facts": proof_facts(node, facts_after),
              "stdout_tail": completed.stdout or "", "stderr_tail": completed.stderr or ""}
    # Only a proved completion is worth reusing. Caching a failure would make a
    # repeat edge into this node replay the same verdict forever: the key holds
    # the head and the upstream outcomes, neither of which a retry changes, so
    # the retry could never observe a cleared transient.
    if cache_key and outcome == "completed":
        _cache_write(repo, cache_key, record)
    return record


def _decode(value: Any) -> str:
    if isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    return value if isinstance(value, str) else ""


def _cache_path(repo: Path, key: str) -> Path:
    return Path(repo) / ".oms" / "graph" / "cache" / ("%s.json" % key)


def _cache_read(repo: Path, key: str) -> Optional[Dict[str, Any]]:
    value = read_json(_cache_path(repo, key), default=None)
    if not isinstance(value, Mapping):
        return None
    outcome = str(value.get("outcome", ""))
    if outcome not in OUTCOMES:
        return None
    return {"claimed_outcome": str(value.get("claimed_outcome", outcome)), "outcome": outcome,
            "detail": "cache-hit", "cached": True, "facts": {},
            "stdout_tail": str(value.get("stdout_tail", "")), "stderr_tail": ""}


def _cache_write(repo: Path, key: str, record: Mapping[str, Any]) -> None:
    atomic_write_json(_cache_path(repo, key), {
        "outcome": record.get("outcome"),
        "claimed_outcome": record.get("claimed_outcome"),
        "stdout_tail": bounded_line(record.get("stdout_tail", ""), TAIL_LIMIT),
        "ts": utc_now(),
    })


def _execute_node(repo: Path, run_id: str, spec: Mapping[str, Any], node_id: str,
                  state: Mapping[str, Any], facts: Mapping[str, Any], options: Mapping[str, Any]) -> None:
    node = spec["nodes"][node_id]
    node_state = state.get("nodes", {}).get(node_id, {})
    attempt = int(node_state.get("attempts", 0) or 0) + 1
    events_module.append_event(repo, run_id, "node_started", node=node_id, attempt=attempt,
                               idempotency_key="start:%s:%d" % (node_id, attempt), actor=dict(RUNNER_ACTOR))
    kind = str(node.get("kind", ""))
    if kind == "agent":
        record = _run_agent(repo, node, options)
    elif kind == "tool":
        record = _run_tool(repo, run_id, spec, node_id, node, attempt, state, facts, options)
    else:
        raise GraphError("node kind %s is not executable" % bounded_line(kind or "(missing)", 40))
    _write_artifact(repo, run_id, node_id, attempt,
                    str(record.get("stdout_tail") or "") or str(record.get("stderr_tail") or ""))
    _append_outcome(repo, run_id, node_id, attempt, record, actor=RUNNER_ACTOR)


# --------------------------------------------------------------------------
# the step loop
# --------------------------------------------------------------------------

def _mirror_journal(repo: Path, run_id: str, status: str) -> None:
    """Best-effort Work Journal mirror; the events file stays authoritative.

    Two shapes the journal owns, not this module: the observer reads its
    source as one JSON object (so the mirror names the run's
    `projection.json`, written immediately before this call, not the
    append-only `events.jsonl`), and `--event-type` comes from the journal's
    own vocabulary — a finished run is the `phase_outcome` that `goal-drive`
    and `patch-land` already record.
    """
    observer = install_root() / "scripts" / "lib" / JOURNAL_OBSERVER
    source = events_module.run_dir(repo, run_id) / "projection.json"
    if not source.is_file():
        return
    try:
        subprocess.run(
            ["bash", "-c", '. "$1"; shift; work_journal_observe "$@"', "oms-graph-run", str(observer),
             str(repo), "graph-run", str(source), "--event-type", JOURNAL_EVENT_TYPE,
             "--source-id", run_id, "--outcome", bounded_line("graph run %s" % status, 200),
             "--outcome-status", status, "--operation", "update"],
            cwd=str(repo), capture_output=True, timeout=JOURNAL_TIMEOUT,
        )
    except Exception:
        return


def _finish(repo: Path, run_id: str, spec: Mapping[str, Any], route: Mapping[str, Any],
            status: str, reason: str) -> Dict[str, Any]:
    rows = events_module.read_events(repo, run_id)
    events_module.append_event(repo, run_id, "run_finished",
                               idempotency_key="finish:%d" % (len(rows) + 1),
                               node=str(route.get("primary") or ""), status=status,
                               actor=dict(RUNNER_ACTOR), detail=bounded_line(reason, DETAIL_LIMIT))
    _project_and_write(repo, run_id, spec)
    _mirror_journal(repo, run_id, status)
    return _result(run_id, spec, status, route, reason)


def _project_and_write(repo: Path, run_id: str, spec: Mapping[str, Any]) -> Dict[str, Any]:
    projection = events_module.project(events_module.read_events(repo, run_id), spec)
    events_module.write_projection(repo, run_id, projection)
    return projection


def _result(run_id: str, spec: Mapping[str, Any], status: str, route: Mapping[str, Any], reason: str) -> Dict[str, Any]:
    return {"schema": EXEC_SCHEMA, "run_id": run_id, "spec_id": str(spec.get("id", "")),
            "status": status, "primary": route.get("primary"), "reason": reason,
            "route": {key: value for key, value in route.items() if key != "trace"}}


def _blocked_reason(selection: Mapping[str, Any]) -> str:
    parts = []
    for item in list(selection.get("conflicts", [])) + list(selection.get("deferred", [])):
        if isinstance(item, Mapping):
            parts.append("%s:%s" % (item.get("node", "?"), item.get("reason", "?")))
    return "nothing is eligible: %s" % ", ".join(parts) if parts else "nothing is eligible"


def _loop(repo: Path, run_id: str, spec: Mapping[str, Any], options: Mapping[str, Any]) -> Dict[str, Any]:
    """Evaluate, schedule, execute, record — until a terminal, gate, or stop."""
    declared = options.get("max_steps") or spec.get("budget", {}).get("max_steps", 20)
    cap = declared if isinstance(declared, int) and not isinstance(declared, bool) and declared > 0 else 20
    jobs = options.get("jobs", 1)
    capacity = jobs if isinstance(jobs, int) and not isinstance(jobs, bool) and jobs > 0 else 1
    route: Dict[str, Any] = {}
    for _step in range(cap):
        rows, state, facts = _read_state(repo, run_id, spec)
        route = route_module.evaluate(spec, state, facts)
        events_module.append_event(repo, run_id, "route_evaluated",
                                   idempotency_key="route:%d" % (len(rows) + 1),
                                   route={key: value for key, value in route.items() if key != "trace"},
                                   budget=dict(route.get("budget", {})), actor=dict(RUNNER_ACTOR))
        status = str(route.get("status", ""))
        if status == "terminal":
            return _finish(repo, run_id, spec, route, "terminal", str(route.get("reason", "")))
        if status == "gate":
            # The parent decides; the run stays open for `decide` then `resume`.
            _project_and_write(repo, run_id, spec)
            return _result(run_id, spec, "gate", route, str(route.get("reason", "")))
        if status != "actionable":
            return _finish(repo, run_id, spec, route, status, str(route.get("reason", "")))
        selection = scheduler.eligible(spec, state, facts, route=route,
                                       task_scopes=_task_scopes(repo, spec), capacity=capacity)
        if not selection["eligible"]:
            return _finish(repo, run_id, spec, route, "blocked", _blocked_reason(selection))
        # `--jobs` only widens the eligible set. Executing the selection
        # concurrently is a non-goal this round: landing is serialized by
        # patch-land regardless, and a sequential loop keeps the event order
        # a faithful record of what actually happened.
        for node_id in selection["eligible"]:
            _execute_node(repo, run_id, spec, node_id, state, facts, options)
        _project_and_write(repo, run_id, spec)
    return _finish(repo, run_id, spec, route, "exhausted", "step cap of %d reached" % cap)


# --------------------------------------------------------------------------
# front doors
# --------------------------------------------------------------------------

def _options(worker: str, model: str, reasoning_effort: str, max_steps: Optional[int], jobs: int, goal: str) -> Dict[str, Any]:
    return {"worker": worker, "model": model, "reasoning_effort": reasoning_effort,
            "max_steps": max_steps, "jobs": jobs, "goal": goal}


def _dry_run(repo: Path, spec: Mapping[str, Any], options: Mapping[str, Any]) -> Dict[str, Any]:
    facts = collect_facts(repo)
    state = route_module.state_from_outcomes(spec, {})
    route = route_module.evaluate(spec, state, facts)
    jobs = options.get("jobs", 1)
    capacity = jobs if isinstance(jobs, int) and not isinstance(jobs, bool) and jobs > 0 else 1
    selection = scheduler.eligible(spec, state, facts, route=route,
                                   task_scopes=_task_scopes(repo, spec), capacity=capacity)
    commands = []
    for node_id in selection["eligible"]:
        node = spec["nodes"][node_id]
        if str(node.get("kind", "")) != "agent":
            continue
        commands.append(plan_adapter.build_command(
            repo, node, provider=str(node.get("provider") or options.get("worker") or ""),
            model=str(options.get("model") or ""), reasoning_effort=str(options.get("reasoning_effort") or ""),
            dry_run=True))
    return {"schema": EXEC_SCHEMA, "dry_run": True, "spec_id": str(spec.get("id", "")),
            "route": {key: value for key, value in route.items() if key != "trace"},
            "eligible": list(selection["eligible"]), "deferred": list(selection["deferred"]),
            "conflicts": list(selection["conflicts"]), "commands": commands}


def run(repo: Path, spec: Mapping[str, Any], *, worker: str, run_id: str = "", model: str = "",
        reasoning_effort: str = "", max_steps: Optional[int] = None, jobs: int = 1, goal: str = "",
        dry_run: bool = False) -> Dict[str, Any]:
    repo = Path(repo).resolve()
    graph = spec_module.load_spec(spec)
    verdict = validate_spec(graph)
    if not verdict["ok"]:
        raise GraphError("cannot run an invalid graph: %s" % ", ".join(item["code"] for item in verdict["errors"]))
    options = _options(worker, model, reasoning_effort, max_steps, jobs, goal)
    if dry_run:
        return _dry_run(repo, graph, options)
    selected = str(run_id or "")
    if selected and (events_module.run_dir(repo, selected) / "graph.json").is_file():
        # Continuing a named run: its frozen spec, not the one just loaded.
        graph = events_module.load_run_spec(repo, selected)
    else:
        selected = events_module.start_run(repo, graph, run_id=selected,
                                           options={"worker": worker, "model": model, "goal": goal})["run_id"]
    return _loop(repo, selected, graph, options)


def _resume_task_id(node: Mapping[str, Any], node_id: str, rows: Sequence[Mapping[str, Any]]) -> str:
    """The plan task an active agent node was working on, from the run's own rows."""
    declared = str(node.get("plan_task", "") or "")
    if declared and declared != plan_adapter.NEXT_TASK:
        return declared
    resolved = ""
    for row in rows:
        if not isinstance(row, Mapping) or row.get("event") != "node_outcome":
            continue
        if str(row.get("node", "")) != node_id:
            continue
        candidate = str(row.get("task_id", "") or "")
        if candidate:
            resolved = candidate
    return resolved


def _reconcile(repo: Path, run_id: str, spec: Mapping[str, Any]) -> None:
    """Give every node the crash left `active` an outcome read from current reality."""
    rows, state, facts = _read_state(repo, run_id, spec)
    for node_id in sorted(state["nodes"]):
        node_state = state["nodes"][node_id]
        if node_state.get("status") != "active":
            continue
        node = spec["nodes"][node_id]
        attempt = int(node_state.get("attempts", 0) or 0) or 1
        record: Dict[str, Any] = {"claimed_outcome": "unverified", "outcome": "unverified",
                                  "detail": "resumed-without-outcome", "facts": proof_facts(node, facts)}
        if str(node.get("kind", "")) == "agent":
            task_id = _resume_task_id(node, node_id, rows)
            if not task_id:
                record["detail"] = "resumed-without-task"
            else:
                try:
                    task = plan_adapter.task_view(repo, task_id)
                except GraphError:
                    record["detail"] = "resumed-task-unreadable"
                else:
                    outcome, absent = plan_adapter.outcome_from_task(node, task, facts)
                    # The recorded claim is a semantic outcome, never a
                    # lifecycle state: an edge only ever matches the former.
                    record["claimed_outcome"] = outcome
                    record["outcome"] = outcome
                    record["task_id"] = task_id
                    record["detail"] = "resumed-from-plan state=%s%s" % (
                        str(task.get("state", "")) or "-",
                        " proof-missing=%s" % ",".join(absent) if absent else "")
        _append_outcome(repo, run_id, node_id, attempt, record, actor=RESUME_ACTOR)


def resume(repo: Path, run_id: str, *, worker: str, **options: Any) -> Dict[str, Any]:
    repo = Path(repo).resolve()
    spec = events_module.load_run_spec(repo, run_id)
    _reconcile(repo, run_id, spec)
    return _loop(repo, run_id, spec, _options(
        worker, str(options.get("model", "") or ""), str(options.get("reasoning_effort", "") or ""),
        options.get("max_steps"), options.get("jobs", 1), str(options.get("goal", "") or "")))


def decide(repo: Path, run_id: str, node: str, outcome: str, *, note: str = "") -> Dict[str, Any]:
    repo = Path(repo).resolve()
    spec = events_module.load_run_spec(repo, run_id)
    node_spec = spec.get("nodes", {}).get(str(node))
    if not isinstance(node_spec, Mapping) or str(node_spec.get("kind", "")) != "gate":
        raise GraphError("only a gate node records a decision, not %s" % bounded_line(node, 60))
    decisions = [str(item) for item in node_spec.get("decisions", [])]
    if str(outcome) not in decisions:
        raise GraphError("gate %s accepts %s, not %s" % (bounded_line(node, 60), ", ".join(decisions), bounded_line(outcome, 40)))
    state = events_module.project(events_module.read_events(repo, run_id), spec)
    attempt = int(state["nodes"][str(node)].get("attempts", 0) or 0) + 1
    row = events_module.append_event(repo, run_id, "gate_decision", node=str(node), attempt=attempt,
                                     idempotency_key="gate:%s:%d" % (node, attempt), outcome=str(outcome),
                                     actor=dict(PARENT_ACTOR), detail=bounded_line(note, DETAIL_LIMIT))
    _project_and_write(repo, run_id, spec)
    _rows, next_state, facts = _read_state(repo, run_id, spec)
    route = route_module.evaluate(spec, next_state, facts)
    return {"schema": EXEC_SCHEMA, "run_id": run_id, "event": row,
            "route": {key: value for key, value in route.items() if key != "trace"}}
