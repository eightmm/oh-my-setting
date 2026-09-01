"""Agent node <-> plan-run adapter (W3).

Two front doors and no others. `plan-run.sh` is the only way this module
advances work, and `agent-plan.sh` is consulted only through the read verbs in
`PLAN_READ_VERBS`. The lifecycle verbs that move a task forward are fenced by
compare-and-set receipts that only `patch-land.sh` can compute, so `_plan_argv`
refuses them outright rather than trusting a caller to stay away.

An outcome is never a worker's claim: `outcome_from_task` reads the stored task
record afterwards, and a claimed completion without its `proof` facts is
downgraded to `unverified`.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import CoreError, bounded_line, install_root, parse_path_list, run_json, safe_id
from oms_runtime.evidence import artifact_rows

from .. import AGENT_MODES
from .. import binding
from .. import predicates
from ..errors import GraphError

# A bare `next` is a read (the plan marks it PLAN_READ_ONLY and mints no
# lease): the graph selects, plan-run owns the lease.
PLAN_READ_VERBS = ("status", "show", "evidence-snapshot", "next")
PROVIDER_RE = re.compile(r"^[A-Za-z0-9._-]{1,40}$")
MODEL_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,80}$")
EFFORT_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_-]{0,20}$")
PLAN_ID_RE = re.compile(r"^plan_[0-9a-f]{32}$")
MAX_REPAIR_ROUNDS = 3
NO_ACTIONABLE_EXIT = 3
PEEK_TIMEOUT = 60
TAIL_LIMIT = 2000


def _script(name: str) -> str:
    return str(install_root() / "scripts" / name)


def _plan_argv(repo: Path, verb: str, *args: str) -> List[str]:
    """Build one read-only agent-plan invocation, refusing every other verb."""
    if verb not in PLAN_READ_VERBS:
        raise GraphError("the graph layer reads plan state only through %s, not %s" % (", ".join(PLAN_READ_VERBS), bounded_line(verb, 40)))
    if any(str(item) == "--claim" for item in args):
        raise GraphError("the graph layer never claims a plan task; plan-run owns the lease")
    argv = ["bash", _script("agent-plan.sh"), "--repo", str(repo), verb]
    argv.extend(str(item) for item in args)
    return argv


def peek_next_task(repo: Path) -> Optional[Dict[str, Any]]:
    """The task `plan-run --next` would claim right now, without claiming it.

    `None` when the plan offers nothing actionable (agent-plan exit 3). Any
    other failure is an error, never a silent "no task".
    """
    argv = _plan_argv(repo, "next", "--json")
    try:
        completed = subprocess.run(argv, cwd=str(repo), capture_output=True, text=True, timeout=PEEK_TIMEOUT)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GraphError("could not peek the plan's next task: %s" % bounded_line(exc, 200))
    if completed.returncode == NO_ACTIONABLE_EXIT:
        return None
    if completed.returncode != 0:
        raise GraphError("agent-plan next failed (exit %d): %s" % (completed.returncode, bounded_line(completed.stderr, 200)))
    try:
        payload = json.loads(completed.stdout or "")
    except ValueError:
        raise GraphError("agent-plan next returned no JSON task")
    if not isinstance(payload, Mapping) or not payload.get("id"):
        raise GraphError("agent-plan next returned no task id")
    _task_id(payload.get("id"))
    return dict(payload)


def plan_status(repo: Path) -> Dict[str, Any]:
    payload = run_json(_plan_argv(repo, "status", "--json"), cwd=repo)
    if payload is None:
        raise GraphError("agent-plan status is unavailable for this repository")
    return payload


def task_view(repo: Path, task_id: str) -> Dict[str, Any]:
    ident = _task_id(task_id)
    payload = run_json(_plan_argv(repo, "show", "--id", ident), cwd=repo)
    if payload is None:
        raise GraphError("plan task is not readable: %s" % ident)
    return payload


def scope_of(task: Mapping[str, Any]) -> Dict[str, List[str]]:
    allowed = task.get("allowed_paths", task.get("allowed", []))
    forbidden = task.get("forbidden_paths", task.get("forbidden", []))
    return {"allowed": parse_path_list(allowed), "forbidden": parse_path_list(forbidden)}


def _task_id(value: Any) -> str:
    try:
        return safe_id(value, "plan task id")
    except CoreError as exc:
        raise GraphError(str(exc))


def _mode_of(node_spec: Mapping[str, Any]) -> str:
    mode = str(node_spec.get("mode", "run") or "run")
    if mode not in AGENT_MODES:
        raise GraphError("agent node mode must be one of %s, not %s" % (", ".join(AGENT_MODES), bounded_line(mode, 40)))
    return mode


def _state_proves_completion(mode: str, state: str, task: Mapping[str, Any], facts: Mapping[str, Any], task_id: str) -> bool:
    """External evidence that the mode's work is finished — never a claim."""
    if mode == "run":
        return state == "review" and bool(task.get("patch"))
    return state == "done" and bool(facts.get("receipt.land.%s.present" % task_id))


def _claimed_outcome(node_spec: Mapping[str, Any], task: Mapping[str, Any], facts: Mapping[str, Any]) -> str:
    """The lifecycle mapping, before the node's own proof predicates apply."""
    mode = _mode_of(node_spec)
    state = str(task.get("state", ""))
    task_id = str(task.get("id", "") or node_spec.get("plan_task", "") or "")

    if state == "blocked":
        return "blocked"
    if _state_proves_completion(mode, state, task, facts, task_id):
        return "completed"
    if state in ("claimed", "running"):
        # The lifecycle says a worker holds it while the process is gone; that
        # is an absence of evidence, not a failure.
        return "unverified"
    if mode == "run" and state == "done":
        # A task landed by another lane still satisfies a run node.
        return "completed"
    return "failed"


def outcome_from_task(node_spec: Mapping[str, Any], task: Mapping[str, Any], facts: Mapping[str, Any]) -> Tuple[str, List[str]]:
    """Map stored plan state to a semantic outcome, then apply the node's proof."""
    outcome = _claimed_outcome(node_spec, task, facts)
    declared = node_spec.get("proof") or []
    proof_missing = list(predicates.missing(list(declared), facts)) if declared else []
    if outcome == "completed" and proof_missing:
        return "unverified", proof_missing
    return outcome, proof_missing


def _check_option(value: Any, pattern: "re.Pattern", label: str) -> str:
    text = str(value or "")
    if not text:
        return ""
    if not pattern.fullmatch(text):
        raise GraphError("%s is not a supported value" % label)
    return text


def build_command(repo: Path, node_spec: Mapping[str, Any], *, provider: str, model: str = "", reasoning_effort: str = "", repair: int = 0, dry_run: bool = False, context_pack: str = "") -> List[str]:
    kind = str(node_spec.get("kind", ""))
    if kind != "agent":
        raise GraphError("only an agent node runs through plan-run, not kind %s" % bounded_line(kind or "(missing)", 40))
    if binding.is_selector(node_spec.get("plan_task")) or node_spec.get("plan_task_from"):
        # Identity is frozen before execution: the runner resolves a selector
        # or a binding to one task id and hands this adapter the concrete node.
        raise GraphError("an agent node reaches plan-run only with a concrete plan task")
    task_id = _task_id(node_spec.get("plan_task", ""))
    mode = _mode_of(node_spec)
    provider_text = str(provider or "")
    # The transport spelling admits '-', so a bare pattern match would let a
    # value that looks like an option through; an argv element never leads with
    # one (the same rule the MCP readers apply to positional values).
    if not PROVIDER_RE.fullmatch(provider_text) or provider_text.startswith("-"):
        raise GraphError("provider must be a registered transport name")
    try:
        rounds = int(repair)
    except (TypeError, ValueError):
        raise GraphError("repair rounds must be an integer")
    if rounds < 0 or rounds > MAX_REPAIR_ROUNDS:
        raise GraphError("repair rounds must be between 0 and %d" % MAX_REPAIR_ROUNDS)
    model_text = _check_option(model, MODEL_RE, "model")
    effort_text = _check_option(reasoning_effort, EFFORT_RE, "reasoning effort")

    argv = ["bash", _script("plan-run.sh"), "--repo", str(repo), "--to", str(provider), "--id", task_id]
    if mode == "land":
        argv.append("--land")
    if rounds > 0:
        argv.extend(["--repair", str(rounds)])
    if model_text:
        argv.extend(["--model", model_text])
    if effort_text:
        argv.extend(["--reasoning-effort", effort_text])
    pack = str(context_pack or "")
    if pack:
        if pack.startswith("-") or "\n" in pack or "\0" in pack:
            raise GraphError("context pack path is not a plain file path")
        argv.extend(["--context-pack", pack])
    if dry_run:
        argv.append("--dry-run")
    return argv


def _plan_id(repo: Path, task_id: str) -> str:
    """The immutable plan lineage; `show` deliberately does not carry it."""
    payload = run_json(_plan_argv(repo, "evidence-snapshot", "--id", task_id), cwd=repo)
    value = str((payload or {}).get("plan_id", "") or "")
    return value if PLAN_ID_RE.fullmatch(value) else ""


def _exit_is_success(row: Mapping[str, Any]) -> bool:
    value = row.get("exit")
    return isinstance(value, int) and not isinstance(value, bool) and value == 0


def _task_facts(repo: Path, task_id: str, task: Mapping[str, Any]) -> Dict[str, Any]:
    """The minimal fact slice this adapter can prove from durable receipts."""
    facts: Dict[str, Any] = {}
    state = str(task.get("state", ""))
    if state:
        facts["plan.task.%s.state" % task_id] = state
    facts["plan.task.%s.patch_present" % task_id] = bool(task.get("patch"))
    facts["plan.task.%s.artifact_present" % task_id] = bool(task.get("artifact"))

    plan_id = _plan_id(repo, task_id)
    landed = False
    admit_latest = ""
    for row in artifact_rows(repo):
        if str(row.get("task_id", "")) != task_id:
            continue
        kind = row.get("kind")
        if kind == "patch-land":
            if _exit_is_success(row) and (not plan_id or str(row.get("plan_id", "")) == plan_id):
                landed = True
        elif kind == "patch-admit":
            admit_latest = "verified" if _exit_is_success(row) else "failed"
    facts["receipt.land.%s.present" % task_id] = landed
    if admit_latest:
        facts["receipt.admit.%s.latest" % task_id] = admit_latest
    return facts


def _tail(value: Any) -> str:
    return bounded_line(value if isinstance(value, str) else (value.decode("utf-8", "replace") if isinstance(value, bytes) else ""), TAIL_LIMIT)


def execute(repo: Path, node_spec: Mapping[str, Any], *, provider: str, model: str = "", reasoning_effort: str = "", repair: int = 0, timeout: int = 2700, dry_run: bool = False, context_pack: str = "") -> Dict[str, Any]:
    """Run one agent node through plan-run and read the outcome off the plan."""
    argv = build_command(repo, node_spec, provider=provider, model=model, reasoning_effort=reasoning_effort, repair=repair, dry_run=dry_run, context_pack=context_pack)
    task_id = _task_id(node_spec.get("plan_task", ""))

    timed_out = False
    try:
        completed = subprocess.run(argv, cwd=str(repo), capture_output=True, text=True, timeout=timeout)
        exit_code = int(completed.returncode)
        stdout_text, stderr_text = completed.stdout, completed.stderr
    except subprocess.TimeoutExpired as exc:
        timed_out = True
        exit_code = -1
        stdout_text, stderr_text = exc.stdout, exc.stderr
    except OSError as exc:
        raise GraphError("plan-run could not be launched: %s" % bounded_line(exc, 200))

    task: Dict[str, Any] = {}
    task_readable = True
    try:
        task = task_view(repo, task_id)
    except GraphError:
        task_readable = False
    facts = _task_facts(repo, task_id, task)

    proof_missing: List[str] = []
    if dry_run:
        claimed = outcome = "skipped"
        reason = "dry-run"
    elif timed_out:
        claimed = outcome = "unverified"
        reason = "timeout"
    elif exit_code == 3:
        # plan-run's "no actionable task" is a plan verdict, not a failure.
        claimed = outcome = "blocked"
        reason = "no-actionable-task"
    elif exit_code == 75:
        # patch-land's non-blocking lock, passed through verbatim; retryable.
        claimed = outcome = "partial"
        reason = "landing-lock-held"
    elif not task_readable:
        claimed = outcome = "unverified"
        reason = "task-unreadable"
    else:
        claimed = _claimed_outcome(node_spec, task, facts)
        outcome, proof_missing = outcome_from_task(node_spec, task, facts)
        reason = ""
        if exit_code != 0:
            state = str(task.get("state", ""))
            mode = _mode_of(node_spec)
            if outcome == "completed" and not _state_proves_completion(mode, state, task, facts, task_id):
                outcome = "failed"
            if outcome == "failed":
                reason = "plan-run-exit-%d" % exit_code

    return {
        "argv": argv,
        "exit": exit_code,
        "stdout_tail": _tail(stdout_text),
        "stderr_tail": _tail(stderr_text),
        "task": task,
        "facts": facts,
        "claimed_outcome": claimed,
        "outcome": outcome,
        "proof_missing": proof_missing,
        "reason": reason,
    }
