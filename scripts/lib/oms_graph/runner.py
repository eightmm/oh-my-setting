"""`exec run` step loop: tool nodes, agent nodes via adapters, gates, caching, resume.

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

Task identity is frozen before work starts. A `plan_task: next` selector is
resolved by one read-only peek, the concrete id goes onto the `node_started`
row (with its `binding` name when the node declares `bind_task`), and only
then does `plan-run --id` run. A wave of eligible nodes executes concurrently
on a thread pool, but this coordinator is the only writer of `events.jsonl`:
threads run subprocesses and hand back records.
"""

from __future__ import annotations

import os
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import (
    CoreError, atomic_write_bytes, atomic_write_json, bounded_line, canonical_json,
    install_root, read_json, sensitive_text, sha256_bytes, sha256_file, utc_now,
)

from . import EXEC_SCHEMA, OUTCOMES
from . import binding
from . import events as events_module
from . import predicates
from . import route as route_module
from . import scheduler
from . import spec as spec_module
from .adapters import plan as plan_adapter
from .errors import GraphError
from .facts import collect_facts
from .project import build as project_build
from .project import context as project_context
from .validate import validate_spec
from .workspace import cache_allowed, workspace_fingerprint

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
GOAL_TOKEN = "${goal}"

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

    Kept for specs written against `plan.task.next.*` / `receipt.*.next.*`;
    new specs bind the selection (`bind_task`) and prove `binding.<name>.*`.
    The alias is derived from the recorded `task_id` (latest row wins) and
    never guessed.
    """
    nodes = spec.get("nodes", {})
    if not isinstance(nodes, Mapping):
        return dict(facts)
    next_nodes = set()
    for node_id, node in nodes.items():
        if isinstance(node, Mapping) and binding.is_selector(node.get("plan_task")):
            next_nodes.add(str(node_id))
    if not next_nodes:
        return dict(facts)
    task_id = ""
    for row in rows:
        if not isinstance(row, Mapping) or row.get("event") not in ("node_started", "node_outcome"):
            continue
        if str(row.get("node", "")) not in next_nodes:
            continue
        candidate = str(row.get("task_id", "") or "")
        if candidate:
            task_id = candidate
    if not task_id or binding.is_selector(task_id):
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


def cache_key(node: Mapping[str, Any], state: Mapping[str, Any], facts: Mapping[str, Any], node_id: str,
              workspace: str) -> str:
    """Execution cache key v2: schema, node, HEAD, workspace fingerprint, upstream outcomes.

    An empty workspace fingerprint means the tree could not be hashed safely
    (symlink, unreadable or oversized dirty file, too many dirty paths); the
    key is then empty and the cache is neither read nor written.
    """
    if not workspace:
        return ""
    node_states = state.get("nodes", {}) if isinstance(state.get("nodes", {}), Mapping) else {}
    upstream = {}
    for other, value in node_states.items():
        if other == node_id or not isinstance(value, Mapping) or value.get("status") != "finished":
            continue
        upstream[str(other)] = value.get("outcome")
    payload = {"schema": EXEC_SCHEMA, "cache": 2, "node": dict(node), "head": facts.get("git.head"),
               "workspace": workspace, "upstream": dict(sorted(upstream.items()))}
    return sha256_bytes(canonical_json(payload))


def _artifact_name(node_id: str, attempt: int) -> str:
    safe = "".join(char if (char.isalnum() or char in "._-") else "_" for char in str(node_id))
    return "%s-%d.txt" % (safe or "node", attempt)


def render_context_task(template: str, goal: str, fallback: str) -> str:
    """`${goal}` is substituted by the runner, never by a shell."""
    text = str(template or "").replace(GOAL_TOKEN, str(goal or "")).strip()
    return text or str(goal or "").strip() or fallback


# --------------------------------------------------------------------------
# run state
# --------------------------------------------------------------------------

def _read_state(repo: Path, run_id: str, spec: Mapping[str, Any]) -> Tuple[List[Dict[str, Any]], Dict[str, Any], Dict[str, Any]]:
    rows = events_module.read_events(repo, run_id)
    state = events_module.project(rows, spec)
    facts = binding.augment_binding_facts(spec, state, alias_facts(collect_facts(repo), rows, spec))
    return rows, state, facts


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
    for key in ("task_id", "binding"):
        value = str(record.get(key, "") or "")
        if value:
            fields[key] = value
    try:
        return events_module.append_event(repo, run_id, "node_outcome", **fields)
    except GraphError:
        fields["detail"] = "detail-withheld"
        return events_module.append_event(repo, run_id, "node_outcome", **fields)


# --------------------------------------------------------------------------
# task resolution: selector -> concrete identity
# --------------------------------------------------------------------------

def _candidates(route: Mapping[str, Any]) -> List[str]:
    names: List[str] = []
    for item in [route.get("primary")] + list(route.get("alternatives") or []):
        name = str(item or "").split(".", 1)[0]
        if name and name not in names:
            names.append(name)
    return names


def _resolve_tasks(repo: Path, spec: Mapping[str, Any], state: Mapping[str, Any], route: Mapping[str, Any],
                   *, strict: bool = True) -> Dict[str, Any]:
    """Concrete task per candidate agent node, before anything is scheduled.

    A literal `plan_task` and a bound `plan_task_from` resolve from the spec
    and the projection; the `next` selector resolves through exactly one
    read-only peek per wave, shared by every selector node so two of them can
    only ever see the same task (and the scheduler then keeps them apart).
    """
    nodes = spec.get("nodes", {})
    resolved: Dict[str, str] = {}
    selectors: List[str] = []
    unavailable: Dict[str, str] = {}
    views: Dict[str, Dict[str, Any]] = {}
    peeked = False
    peek: Optional[Dict[str, Any]] = None
    peek_error = ""
    for node_id in _candidates(route):
        node = nodes.get(node_id)
        if not isinstance(node, Mapping) or node.get("kind") != "agent":
            continue
        static = binding.resolve_static(node, state)
        if static:
            resolved[node_id] = static
            continue
        if not binding.is_selector(node.get("plan_task")):
            continue
        selectors.append(node_id)
        if not peeked:
            peeked = True
            try:
                peek = plan_adapter.peek_next_task(repo)
            except GraphError as exc:
                if strict:
                    raise
                peek_error = bounded_line(exc, 200)
        if peek is not None:
            task_id = str(peek.get("id"))
            resolved[node_id] = task_id
            views[task_id] = dict(peek)
        else:
            unavailable[node_id] = peek_error or "no-actionable-task"
    # Active nodes hold their recorded task; the scheduler needs it for the
    # same-task rule.
    for node_id, node_state in (state.get("nodes", {}) or {}).items():
        if isinstance(node_state, Mapping) and node_state.get("status") == "active" and node_state.get("task_id"):
            resolved.setdefault(str(node_id), str(node_state["task_id"]))
    return {"resolved": resolved, "selectors": selectors, "unavailable": unavailable, "views": views}


def _task_scopes(repo: Path, resolution: Mapping[str, Any]) -> Dict[str, Dict[str, List[str]]]:
    """Declared write scope per concrete task; an unreadable task is left out."""
    scopes: Dict[str, Dict[str, List[str]]] = {}
    views = resolution.get("views", {})
    for task_id in sorted(set(resolution.get("resolved", {}).values())):
        if task_id in scopes:
            continue
        view = views.get(task_id)
        try:
            scopes[task_id] = plan_adapter.scope_of(view if isinstance(view, Mapping) else plan_adapter.task_view(repo, task_id))
        except GraphError:
            # The scheduler reports `unknown-scope` rather than guessing.
            continue
    return scopes


def _active_nodes(state: Mapping[str, Any]) -> List[str]:
    nodes = state.get("nodes", {}) if isinstance(state.get("nodes", {}), Mapping) else {}
    return sorted(str(node_id) for node_id, value in nodes.items()
                  if isinstance(value, Mapping) and value.get("status") == "active")


# --------------------------------------------------------------------------
# context bridge: project graph -> pack -> plan-run
# --------------------------------------------------------------------------

def prepare_context(repo: Path, node: Mapping[str, Any], node_id: str, goal: str) -> Optional[Dict[str, Any]]:
    """Build the agent node's context pack; the project graph is a regenerable
    cache, so an absent or stale graph is rebuilt here. Orientation only: the
    pack never widens the task's authority, and a failure to build it is
    recorded, not fatal."""
    context = node.get("context")
    if not isinstance(context, Mapping):
        return None
    task = render_context_task(str(context.get("task", "")), goal, str(node.get("title") or node_id))
    max_files = context.get("max_files", 12)
    if isinstance(max_files, bool) or not isinstance(max_files, int) or max_files <= 0:
        max_files = 12
    try:
        project_build.ensure(repo)
        graph = project_build.load_graph(repo)
        pack = project_context.context_pack(repo, graph, task=task, max_files=max_files)
        path = project_build.state_dir(repo) / "context" / ("%s.json" % pack["pack_digest"])
        digest = sha256_file(path)
    except (CoreError, OSError, KeyError, ValueError) as exc:
        return {"status": "unavailable", "reason": bounded_line(exc, 200)}
    return {
        "status": "ready",
        "path": str(path),
        "project_graph_revision": str(graph.get("revision", "") or ""),
        "context_pack_sha256": digest,
        "context_file_count": len(pack.get("files", [])),
    }


def _context_fields(context: Optional[Mapping[str, Any]]) -> Dict[str, Any]:
    if not context:
        return {}
    if context.get("status") != "ready":
        return {"context": {"status": "unavailable", "reason": str(context.get("reason", ""))}}
    return {key: context[key] for key in ("project_graph_revision", "context_pack_sha256", "context_file_count")}


# --------------------------------------------------------------------------
# node execution (thread side: subprocesses only; no event writes)
# --------------------------------------------------------------------------

def _run_agent(repo: Path, node: Mapping[str, Any], options: Mapping[str, Any], context_pack: str) -> Dict[str, Any]:
    result = plan_adapter.execute(
        repo, node,
        provider=str(node.get("provider") or options.get("worker") or ""),
        model=str(options.get("model") or ""),
        reasoning_effort=str(options.get("reasoning_effort") or ""),
        timeout=int(node.get("timeout", AGENT_TIMEOUT) or AGENT_TIMEOUT),
        context_pack=context_pack,
    )
    record = agent_outcome(result)
    task = result.get("task")
    if isinstance(task, Mapping) and task.get("id"):
        record["task_id"] = str(task.get("id"))
    return record


def _tool_env(run_id: str, node_id: str, attempt: int, goal: str, bindings: Mapping[str, Any]) -> Dict[str, str]:
    """Run identity for the command, plus each task binding as
    `OMS_GRAPH_TASK_<NAME>` so a tool (the exact commit step, an inspection)
    can name the bound task without parsing the run."""
    env = dict(os.environ)
    env.update({
        "OMS_GRAPH_RUN_ID": run_id,
        "OMS_GRAPH_NODE": node_id,
        "OMS_GRAPH_ATTEMPT": str(attempt),
        "OMS_GRAPH_GOAL": str(goal or ""),
    })
    for name, entry in sorted(bindings.items()):
        task_id = str(entry.get("task_id", "") or "") if isinstance(entry, Mapping) else ""
        if task_id and binding.valid_name(name):
            env["OMS_GRAPH_TASK_%s" % str(name).upper().replace("-", "_")] = task_id
    return env


def _run_tool_process(repo: Path, run_id: str, node_id: str, node: Mapping[str, Any], attempt: int,
                      options: Mapping[str, Any], bindings: Mapping[str, Any]) -> Dict[str, Any]:
    """The subprocess half of a tool node; the proof check happens on the coordinator."""
    command = str(node.get("command", "") or "")
    if not command:
        raise GraphError("tool node %s has no command" % bounded_line(node_id, 60))
    try:
        completed = subprocess.run(
            ["bash", "-c", command], cwd=str(repo),
            env=_tool_env(run_id, node_id, attempt, str(options.get("goal", "")), bindings),
            capture_output=True, text=True,
            timeout=int(node.get("timeout", TOOL_TIMEOUT) or TOOL_TIMEOUT),
        )
    except subprocess.TimeoutExpired as exc:
        return {"timeout": True, "exit": -1, "stdout_tail": _decode(exc.stdout), "stderr_tail": _decode(exc.stderr)}
    except OSError as exc:
        raise GraphError("tool node %s could not run: %s" % (bounded_line(node_id, 60), bounded_line(exc, 200)))
    return {"timeout": False, "exit": int(completed.returncode),
            "stdout_tail": completed.stdout or "", "stderr_tail": completed.stderr or ""}


def _finish_tool(repo: Path, run_id: str, spec: Mapping[str, Any], node: Mapping[str, Any],
                 raw: Mapping[str, Any], facts_before: Mapping[str, Any], key: str) -> Dict[str, Any]:
    """Coordinator half: proof against facts read after the command, then cache."""
    if raw.get("timeout"):
        return {"claimed_outcome": "unverified", "outcome": "unverified", "detail": "timeout",
                "facts": proof_facts(node, facts_before),
                "stdout_tail": str(raw.get("stdout_tail", "")), "stderr_tail": str(raw.get("stderr_tail", ""))}
    exit_code = int(raw.get("exit", 1))
    claimed = "completed" if exit_code == 0 else "failed"
    facts_after = _read_state(repo, run_id, spec)[2]
    outcome, absent = route_module.effective_outcome(node, claimed, facts_after)
    detail = "exit=%d" % exit_code
    if absent:
        detail = "%s proof-missing=%s" % (detail, ",".join(absent))
    record = {"claimed_outcome": claimed, "outcome": outcome, "detail": detail,
              "facts": proof_facts(node, facts_after),
              "stdout_tail": str(raw.get("stdout_tail", "")), "stderr_tail": str(raw.get("stderr_tail", ""))}
    # Only a proved completion is worth reusing. Caching a failure would make a
    # repeat edge into this node replay the same verdict forever: the key holds
    # the head, the workspace and the upstream outcomes, none of which a retry
    # changes, so the retry could never observe a cleared transient.
    if key and outcome == "completed":
        _cache_write(repo, key, record)
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


def _workspace_digest(repo: Path) -> str:
    try:
        fingerprint = workspace_fingerprint(repo)
    except GraphError:
        return ""
    return str(fingerprint.get("digest", "")) if cache_allowed(fingerprint) else ""


# --------------------------------------------------------------------------
# a wave: started rows first, then concurrent work, then outcomes
# --------------------------------------------------------------------------

def _plan_node(repo: Path, run_id: str, spec: Mapping[str, Any], node_id: str, state: Mapping[str, Any],
               facts: Mapping[str, Any], options: Mapping[str, Any], resolution: Mapping[str, Any],
               wave_id: str, wave_index: int, workspace: str) -> Dict[str, Any]:
    """Freeze one node's identity and context, append its `node_started`, and
    return the work item a thread will execute."""
    node = spec["nodes"][node_id]
    node_state = state.get("nodes", {}).get(node_id, {})
    attempt = int(node_state.get("attempts", 0) or 0) + 1
    fields: Dict[str, Any] = {"node": node_id, "attempt": attempt,
                              "idempotency_key": "start:%s:%d" % (node_id, attempt),
                              "actor": dict(RUNNER_ACTOR), "wave_id": wave_id, "wave_index": wave_index}
    item: Dict[str, Any] = {"node_id": node_id, "node": node, "attempt": attempt, "kind": str(node.get("kind", "")),
                            "task_id": "", "binding": "", "context_pack": "", "cache_key": "", "cached": None,
                            "bindings": dict(state.get("bindings", {}) or {})}
    if item["kind"] == "agent":
        task_id = str(resolution.get("resolved", {}).get(node_id, "") or "")
        if not task_id:
            raise GraphError("agent node %s has no concrete plan task" % bounded_line(node_id, 60))
        item["task_id"] = fields["task_id"] = task_id
        name = str(node.get("bind_task") or "")
        if name:
            item["binding"] = fields["binding"] = name
        item["node"] = binding.effective_node(node, task_id)
        context = prepare_context(repo, node, node_id, str(options.get("goal", "")))
        fields.update(_context_fields(context))
        if context and context.get("status") == "ready":
            item["context_pack"] = str(context["path"])
    elif item["kind"] == "tool":
        if _cacheable(node):
            item["cache_key"] = cache_key(node, state, facts, node_id, workspace)
            if item["cache_key"]:
                hit = _cache_read(repo, item["cache_key"])
                if hit is not None:
                    hit["facts"] = proof_facts(node, facts)
                    item["cached"] = hit
    else:
        raise GraphError("node kind %s is not executable" % bounded_line(item["kind"] or "(missing)", 40))
    events_module.append_event(repo, run_id, "node_started", **fields)
    return item


def _work(repo: Path, run_id: str, item: Mapping[str, Any], options: Mapping[str, Any]) -> Dict[str, Any]:
    """Thread body: subprocesses only, never an event write."""
    if item["cached"] is not None:
        return dict(item["cached"])
    if item["kind"] == "agent":
        return _run_agent(repo, item["node"], options, str(item["context_pack"]))
    return _run_tool_process(repo, run_id, str(item["node_id"]), item["node"], int(item["attempt"]), options,
                             item.get("bindings") or {})


def _record(repo: Path, run_id: str, spec: Mapping[str, Any], item: Mapping[str, Any], raw: Mapping[str, Any],
            facts: Mapping[str, Any]) -> None:
    node_id, attempt = str(item["node_id"]), int(item["attempt"])
    if item["kind"] == "tool" and item["cached"] is None:
        record = _finish_tool(repo, run_id, spec, item["node"], raw, facts, str(item["cache_key"]))
    else:
        record = dict(raw)
    if item["task_id"]:
        record.setdefault("task_id", item["task_id"])
    if item["binding"]:
        record["binding"] = item["binding"]
    _write_artifact(repo, run_id, node_id, attempt,
                    str(record.get("stdout_tail") or "") or str(record.get("stderr_tail") or ""))
    _append_outcome(repo, run_id, node_id, attempt, record, actor=RUNNER_ACTOR)


def _runner_error(item: Mapping[str, Any], exc: BaseException) -> Dict[str, Any]:
    """A node whose work could not even start is unverified, never left active."""
    return {"claimed_outcome": "unverified", "outcome": "unverified",
            "detail": bounded_line("runner-error: %s" % exc, DETAIL_LIMIT), "facts": {},
            "stdout_tail": "", "stderr_tail": "", "timeout": False, "exit": -1}


def _execute_wave(repo: Path, run_id: str, spec: Mapping[str, Any], eligible: Sequence[str],
                  state: Mapping[str, Any], facts: Mapping[str, Any], options: Mapping[str, Any],
                  resolution: Mapping[str, Any], wave_id: str, capacity: int) -> None:
    workspace = _workspace_digest(repo) if any(_cacheable(spec["nodes"][name]) for name in eligible) else ""
    items = [_plan_node(repo, run_id, spec, node_id, state, facts, options, resolution, wave_id, index, workspace)
             for index, node_id in enumerate(eligible)]
    if len(items) == 1:
        item = items[0]
        try:
            raw = _work(repo, run_id, item, options)
        except (GraphError, CoreError) as exc:
            raw = _runner_error(item, exc)
        _record(repo, run_id, spec, item, raw, facts)
        return
    with ThreadPoolExecutor(max_workers=max(1, min(capacity, len(items)))) as pool:
        futures = {pool.submit(_work, repo, run_id, item, options): item for item in items}
        for future in as_completed(futures):
            item = futures[future]
            try:
                raw = future.result()
            except (GraphError, CoreError) as exc:
                raw = _runner_error(item, exc)
            _record(repo, run_id, spec, item, raw, facts)


def _record_unavailable(repo: Path, run_id: str, spec: Mapping[str, Any], node_id: str, state: Mapping[str, Any],
                        facts: Mapping[str, Any], reason: str, wave_id: str) -> None:
    """A selector with nothing to select: the plan's verdict, recorded as blocked."""
    node = spec["nodes"][node_id]
    attempt = int(state.get("nodes", {}).get(node_id, {}).get("attempts", 0) or 0) + 1
    events_module.append_event(repo, run_id, "node_started", node=node_id, attempt=attempt,
                               idempotency_key="start:%s:%d" % (node_id, attempt), actor=dict(RUNNER_ACTOR),
                               wave_id=wave_id, wave_index=0)
    _append_outcome(repo, run_id, node_id, attempt, {
        "claimed_outcome": "blocked", "outcome": "blocked", "detail": bounded_line(reason, DETAIL_LIMIT),
        "facts": proof_facts(node, facts)}, actor=RUNNER_ACTOR)


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
    projection = _project_and_write(repo, run_id, spec)
    _mirror_journal(repo, run_id, status)
    return _result(run_id, spec, status, route, reason, projection)


def _project_and_write(repo: Path, run_id: str, spec: Mapping[str, Any]) -> Dict[str, Any]:
    projection = events_module.project(events_module.read_events(repo, run_id), spec)
    events_module.write_projection(repo, run_id, projection)
    return projection


def _result(run_id: str, spec: Mapping[str, Any], status: str, route: Mapping[str, Any], reason: str,
            projection: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    return {"schema": EXEC_SCHEMA, "run_id": run_id, "spec_id": str(spec.get("id", "")),
            "status": status, "primary": route.get("primary"), "reason": reason,
            "bindings": dict((projection or {}).get("bindings", {}) or {}),
            "route": {key: value for key, value in route.items() if key != "trace"}}


def _blocked_reason(selection: Mapping[str, Any]) -> str:
    parts = []
    for item in list(selection.get("conflicts", [])) + list(selection.get("deferred", [])):
        if isinstance(item, Mapping):
            parts.append("%s:%s" % (item.get("node", "?"), item.get("reason", "?")))
    return "nothing is eligible: %s" % ", ".join(parts) if parts else "nothing is eligible"


def _loop(repo: Path, run_id: str, spec: Mapping[str, Any], options: Mapping[str, Any]) -> Dict[str, Any]:
    """Evaluate, resolve, schedule, execute, record — until a terminal, gate, or stop."""
    declared = options.get("max_steps") or spec.get("budget", {}).get("max_steps", 20)
    cap = declared if isinstance(declared, int) and not isinstance(declared, bool) and declared > 0 else 20
    jobs = options.get("jobs", 1)
    capacity = jobs if isinstance(jobs, int) and not isinstance(jobs, bool) and jobs > 0 else 1
    route: Dict[str, Any] = {}
    for step in range(cap):
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
            projection = _project_and_write(repo, run_id, spec)
            return _result(run_id, spec, "gate", route, str(route.get("reason", "")), projection)
        if status != "actionable":
            return _finish(repo, run_id, spec, route, status, str(route.get("reason", "")))
        wave_id = "wave-%d" % (len(rows) + 1)
        resolution = _resolve_tasks(repo, spec, state, route)
        if resolution["unavailable"]:
            # The plan offered no task to a selector: that verdict is recorded
            # for the node and the route re-evaluated, never re-selected.
            for node_id in sorted(resolution["unavailable"]):
                _record_unavailable(repo, run_id, spec, node_id, state, facts, resolution["unavailable"][node_id], wave_id)
            _project_and_write(repo, run_id, spec)
            continue
        selection = scheduler.eligible(spec, state, facts, route=route,
                                       task_scopes=_task_scopes(repo, resolution), capacity=capacity,
                                       active=_active_nodes(state), resolved_tasks=resolution["resolved"],
                                       selectors=resolution["selectors"])
        if not selection["eligible"]:
            return _finish(repo, run_id, spec, route, "blocked", _blocked_reason(selection))
        _execute_wave(repo, run_id, spec, selection["eligible"], state, facts, options, resolution, wave_id, capacity)
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
    resolution = _resolve_tasks(repo, spec, state, route, strict=False)
    selection = scheduler.eligible(spec, state, facts, route=route,
                                   task_scopes=_task_scopes(repo, resolution), capacity=capacity,
                                   resolved_tasks=resolution["resolved"], selectors=resolution["selectors"])
    commands = []
    for node_id in selection["eligible"]:
        node = spec["nodes"][node_id]
        if str(node.get("kind", "")) != "agent":
            continue
        commands.append(plan_adapter.build_command(
            repo, binding.effective_node(node, resolution["resolved"][node_id]),
            provider=str(node.get("provider") or options.get("worker") or ""),
            model=str(options.get("model") or ""), reasoning_effort=str(options.get("reasoning_effort") or ""),
            dry_run=True))
    return {"schema": EXEC_SCHEMA, "dry_run": True, "spec_id": str(spec.get("id", "")),
            "route": {key: value for key, value in route.items() if key != "trace"},
            "resolved_tasks": dict(sorted(resolution["resolved"].items())),
            "unavailable": dict(sorted(resolution["unavailable"].items())),
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


def _reconcile(repo: Path, run_id: str, spec: Mapping[str, Any]) -> None:
    """Give every node the crash left `active` an outcome read from current reality.

    An agent node's task is the identity its own `node_started` row froze. A
    task another lane still holds under a live lease stays `active` (the route
    reports `waiting`); an expired claim is absence of evidence, `unverified`,
    for the repeat or recovery route to judge. Nothing here touches a lease.
    A write tool that died mid-run is `blocked`: rerunning it blindly could
    repeat a side effect, so that decision is the operator's.
    """
    rows, state, facts = _read_state(repo, run_id, spec)
    for node_id in sorted(state["nodes"]):
        node_state = state["nodes"][node_id]
        if node_state.get("status") != "active":
            continue
        node = spec["nodes"][node_id]
        attempt = int(node_state.get("attempts", 0) or 0) or 1
        kind = str(node.get("kind", ""))
        record: Dict[str, Any] = {"claimed_outcome": "unverified", "outcome": "unverified",
                                  "detail": "resumed-without-outcome", "facts": proof_facts(node, facts)}
        if kind == "tool" and str(node.get("effect", "read")) == "write":
            record["claimed_outcome"] = record["outcome"] = "blocked"
            record["detail"] = "resumed-write-tool-uncertain"
        elif kind == "agent":
            task_id = str(node_state.get("task_id", "") or "") or (binding.resolve_static(node, state) or "")
            name = str(node.get("bind_task") or "")
            if not task_id:
                record["detail"] = "resumed-without-task"
            else:
                try:
                    task = plan_adapter.task_view(repo, task_id)
                except GraphError:
                    record["detail"] = "resumed-task-unreadable"
                    record["task_id"] = task_id
                else:
                    task_state = str(task.get("state", ""))
                    if task_state in ("claimed", "running") and not bool(task.get("claim_expired", False)):
                        # A live lease elsewhere: the run keeps waiting for it.
                        continue
                    concrete = binding.effective_node(node, task_id)
                    outcome, absent = plan_adapter.outcome_from_task(concrete, task, facts)
                    # The recorded claim is a semantic outcome, never a
                    # lifecycle state: an edge only ever matches the former.
                    record["claimed_outcome"] = outcome
                    record["outcome"] = outcome
                    record["task_id"] = task_id
                    if name:
                        record["binding"] = name
                    record["facts"] = proof_facts(concrete, facts)
                    record["detail"] = "resumed-from-plan state=%s%s%s" % (
                        task_state or "-",
                        " claim-expired" if bool(task.get("claim_expired", False)) else "",
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
