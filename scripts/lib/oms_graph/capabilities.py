"""Typed tool capabilities: the spec names an intent, the runner builds the argv.

A raw `command` tool node declares `effect: read`, and nothing checks that
the shell text honours it — acceptable for operator-authored specs, not for a
GraphSpec a planner will one day generate. A capability node
(`{"kind": "tool", "tool": "plan_acceptance"}`) carries no shell at all: this
registry owns the front door, its effect, its parameters, and the exact argv,
so the declaration and the behaviour cannot drift apart. The bundled specs
use capabilities only; `command` nodes remain legal and get a validator
warning instead of a refusal.

Front doors named here, and no others: `agent-plan.sh accept` (runs the plan's
own acceptance contract and records its receipt — not a lifecycle mutation),
`graph.sh project context` (a regenerable orientation pack), and
`graph.sh exec commit` (the exact, parent-only commit of a bound task).
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Tuple

from oms_runtime.common import install_root

from . import binding
from .errors import GraphError

GOAL_PLACEHOLDER = "${goal}"
MAX_CONTEXT_FILES = 200

# name -> effect, whether the result may be cached, parameter schema
# (field -> (type name, required)), and a one-line description.
REGISTRY: Dict[str, Dict[str, Any]] = {
    "plan_acceptance": {
        "effect": "read", "cacheable": False, "params": {},
        "description": "run the plan's acceptance contract (agent-plan accept) and record its receipt",
    },
    "project_context": {
        "effect": "read", "cacheable": True,
        "params": {"task": ("str", False), "max_files": ("int", False)},
        "description": "build a project-graph context pack for a task (graph project context --json)",
    },
    "commit_bound": {
        "effect": "write", "cacheable": False,
        "params": {"binding": ("binding", True)},
        "description": "commit exactly the landed patch of a bound task (graph exec commit)",
    },
}


def names() -> List[str]:
    return sorted(REGISTRY)


def is_capability_node(node: Mapping[str, Any]) -> bool:
    return str(node.get("kind", "")) == "tool" and isinstance(node.get("tool"), str) and bool(node.get("tool"))


def render_goal(template: str, goal: str, fallback: str) -> str:
    """`${goal}` is substituted by the runner, never by a shell."""
    text = str(template or "").replace(GOAL_PLACEHOLDER, str(goal or "")).strip()
    return text or str(goal or "").strip() or fallback


def validate_node(node: Mapping[str, Any]) -> List[Tuple[str, str, str]]:
    """(code, field, message) problems for one capability node; pure."""
    problems: List[Tuple[str, str, str]] = []
    name = str(node.get("tool", "") or "")
    entry = REGISTRY.get(name)
    if entry is None:
        return [("unknown_tool_capability", "tool", "unknown tool capability %s; known: %s" % (name, ", ".join(names())))]
    if isinstance(node.get("command"), str) and node.get("command", "").strip():
        problems.append(("invalid_command", "command", "a tool node names a capability or a command, not both"))
    declared = node.get("effect")
    if declared is not None and declared != entry["effect"]:
        problems.append(("invalid_effect", "effect", "capability %s has effect %s, not %s" % (name, entry["effect"], declared)))
    if node.get("cacheable") and not entry["cacheable"]:
        problems.append(("invalid_command", "cacheable", "capability %s is not cacheable" % name))
    for field, (kind, required) in entry["params"].items():
        value = node.get(field)
        if value is None:
            if required:
                problems.append(("invalid_command", field, "capability %s requires %s" % (name, field)))
            continue
        if kind == "str" and not isinstance(value, str):
            problems.append(("invalid_command", field, "%s must be a string" % field))
        elif kind == "int" and (isinstance(value, bool) or not isinstance(value, int) or value <= 0 or value > MAX_CONTEXT_FILES):
            problems.append(("invalid_command", field, "%s must be an integer from 1 to %d" % (field, MAX_CONTEXT_FILES)))
        elif kind == "binding" and not binding.valid_name(value):
            problems.append(("invalid_task_binding", field, "%s must be a binding identifier" % field))
    return problems


def _script(name: str) -> str:
    return str(install_root() / "scripts" / name)


def argv(repo: Path, node: Mapping[str, Any], *, goal: str = "", run_id: str = "", fallback_task: str = "") -> List[str]:
    """The exact command for a capability node. No shell is involved."""
    problems = validate_node(node)
    if problems:
        raise GraphError("tool capability is invalid: %s" % problems[0][2])
    name = str(node["tool"])
    repo_text = str(Path(repo))
    if name == "plan_acceptance":
        return ["bash", _script("agent-plan.sh"), "--repo", repo_text, "accept"]
    if name == "project_context":
        task = render_goal(str(node.get("task", GOAL_PLACEHOLDER)), goal, fallback_task or "repository orientation")
        max_files = node.get("max_files", 12)
        return ["bash", _script("graph.sh"), "--repo", repo_text, "project", "context",
                "--task", task, "--max-files", str(max_files), "--json"]
    if name == "commit_bound":
        command = ["bash", _script("graph.sh"), "--repo", repo_text, "exec", "commit", "--binding", str(node["binding"])]
        if run_id:
            command.extend(["--run", run_id])
        return command
    raise GraphError("tool capability %s has no argv builder" % name)
