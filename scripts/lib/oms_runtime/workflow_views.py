"""Bounded views of an existing TaskEnvelope; never a second authority engine."""
from __future__ import annotations

from collections import Counter
import re
import unicodedata

MAX_ROWS = 4096
MAX_EDGES = 65536
DIGEST = re.compile(r"[0-9a-f]{64}\Z")
PLAN_ID = re.compile(r"plan_[0-9a-f]{32}\Z")
STATUSES = {"verified", "failed", "missing", "stale", "inconclusive", "skipped_with_reason"}


class ViewError(ValueError):
    """Invalid or excessive input, with content-free error messages."""


def text(value, maximum=500):
    if not isinstance(value, str) or len(value) > 65536:
        raise ViewError("invalid or excessive text in workflow projection")
    cleaned = "".join(" " if c.isspace() else "?" if unicodedata.category(c)[0] == "C" else c for c in value)
    return cleaned if len(cleaned) <= maximum else cleaned[:maximum] + " [shortened]"


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9._:-]{1,160}", value):
        raise ViewError("invalid workflow identifier")
    return value


def rows(value, maximum=MAX_ROWS):
    if not isinstance(value, list) or len(value) > maximum or any(not isinstance(r, dict) for r in value):
        raise ViewError("invalid or excessive workflow rows")
    return value


def limit_value(value):
    if type(value) is not int or not 1 <= value <= 100:
        raise ViewError("limit must be an integer from 1 through 100")
    return value


def bounded(values, limit):
    limit_value(limit)
    return {"total": len(values), "items": values[:limit], "omitted": max(0, len(values) - limit)}


def index(values):
    result = {}
    for row in rows(values):
        key = identifier(row.get("id"))
        if key in result:
            raise ViewError("duplicate workflow identifier")
        result[key] = row
    return result


def validate_envelope(value):
    if not isinstance(value, dict) or type(value.get("schema")) is not int or value["schema"] != 2:
        raise ViewError("expected a schema-2 TaskEnvelope")
    digest = value.get("state_digest")
    if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
        raise ViewError("invalid TaskEnvelope state digest")
    for key in ("plan", "task", "evidence", "repo"):
        if not isinstance(value.get(key), dict):
            raise ViewError("incomplete TaskEnvelope")
    criteria = index(value.get("criteria"))
    tasks = index(value["plan"].get("tasks", []))
    edges = 0
    for task in tasks.values():
        dependencies = task.get("depends", [])
        if not isinstance(dependencies, list) or len(dependencies) > MAX_ROWS:
            raise ViewError("invalid task dependencies")
        for dependency in dependencies:
            identifier(dependency)
        if len(set(dependencies)) != len(dependencies):
            raise ViewError("duplicate task dependency")
        edges += len(dependencies)
        text(task.get("state", "unknown"), 80)
        text(task.get("title", ""))
    if edges > MAX_EDGES:
        raise ViewError("dependency graph exceeds the read budget")
    for criterion in criteria.values():
        status = criterion.get("status")
        if not isinstance(status, str) or status not in STATUSES:
            raise ViewError("invalid criterion evidence status")
        text(criterion.get("text", ""))
        text(criterion.get("source", "unknown"), 80)
        for field in ("plan_task_id", "active_task_id"):
            link_value = criterion.get(field)
            if link_value is not None and (not isinstance(link_value, str) or link_value and identifier(link_value) != link_value):
                raise ViewError("invalid criterion task linkage")
        for item in rows(criterion.get("evidence", []), 512):
            status = item.get("status")
            if not isinstance(status, str) or status not in STATUSES:
                raise ViewError("invalid evidence item status")
            for key in ("evidence_ref", "kind", "source", "ts", "signal", "scope_digest"):
                text(item.get(key, ""))
    rows(value.get("next_actions", []), 128)
    rows(value.get("failures", []), MAX_ROWS)
    return criteria, tasks


def _evidence_item(item):
    return {key: text(item.get(key, ""), 300) for key in
            ("evidence_ref", "kind", "status", "signal", "source", "ts", "scope_digest")}


def evidence_matrix(envelope, *, limit=20):
    """Keep explicit criterion lineage; missing task links are never guessed."""
    limit_value(limit)
    criteria, tasks = validate_envelope(envelope)
    plan_id = envelope["plan"].get("plan_id")
    result = []
    for key, criterion in criteria.items():
        linked = None
        source = criterion.get("source")
        relation = "not-recorded"
        if source == "plan-task":
            task_id = criterion.get("plan_task_id")
            if (isinstance(plan_id, str) and PLAN_ID.fullmatch(plan_id)
                    and criterion.get("plan_id") == plan_id and isinstance(task_id, str) and task_id in tasks):
                task = tasks[task_id]
                linked = {"kind": "plan-task", "id": task_id, "state": text(task.get("state", "unknown"), 80),
                          "verify_digest": text(task.get("verify_digest", ""), 80)}
                relation = "explicit-plan-lineage"
            else:
                relation = "invalid-plan-lineage"
        elif source == "task":
            task_id = criterion.get("active_task_id")
            task = envelope["task"]
            if isinstance(task_id, str) and task_id and task_id == task.get("task_id"):
                linked = {"kind": "active-task", "id": identifier(task_id),
                          "state": text(task.get("status", "unknown"), 80),
                          "verify_digest": text(task.get("verify_digest", ""), 80)}
                relation = "explicit-active-task"
            else:
                relation = "invalid-active-task-lineage"
        items = [_evidence_item(row) for row in criterion.get("evidence", [])]
        result.append({"id": key, "text": text(criterion.get("text", "")),
                       "source": text(source or "unknown", 80), "reported_status": criterion["status"],
                       "task_link": relation, "task": linked, "evidence": bounded(items, limit)})
    priority = {"failed": 0, "stale": 1, "missing": 2, "inconclusive": 3,
                "skipped_with_reason": 4, "verified": 5}
    result.sort(key=lambda row: (priority[row["reported_status"]], row["id"]))
    return {"schema": 1, "kind": "oms-evidence-matrix", "state_digest": envelope["state_digest"],
            "authority": "read-projection-only", "counts": dict(Counter(c["reported_status"] for c in result)),
            "criteria": bounded(result, limit), "unlinked_criteria": sum(c["task_link"] == "not-recorded" for c in result),
            "invalid_links": sum(c["task_link"].startswith("invalid-") for c in result),
            "note": "Statuses come from the canonical envelope. No assignments, verifications, approvals or landing receipts are created. Evidence lists inherit the upstream retained-window limit."}


def _cycle_members(tasks):
    """Iterative SCC traversal stays linear even for many overlapping cycles."""
    edges = {key: [d for d in row.get("depends", []) if d in tasks] for key, row in tasks.items()}
    reverse = {key: [] for key in tasks}
    for key, dependencies in edges.items():
        for dependency in dependencies:
            reverse[dependency].append(key)
    seen, order = set(), []
    for start in tasks:
        if start in seen:
            continue
        seen.add(start)
        stack = [(start, iter(edges[start]))]
        while stack:
            node, children = stack[-1]
            child = next(children, None)
            if child is None:
                stack.pop(); order.append(node)
            elif child not in seen:
                seen.add(child); stack.append((child, iter(edges[child])))
    assigned, members = set(), set()
    for start in reversed(order):
        if start in assigned:
            continue
        assigned.add(start); component = []; stack = [start]
        while stack:
            node = stack.pop(); component.append(node)
            for parent in reverse[node]:
                if parent not in assigned:
                    assigned.add(parent); stack.append(parent)
        if len(component) > 1 or start in edges[start]:
            members.update(component)
    return members


def explain_next(envelope, *, task_id=None, limit=20):
    """Explain recorded blockers without scheduling or granting authority."""
    limit_value(limit)
    criteria, tasks = validate_envelope(envelope)
    if task_id is not None and (identifier(task_id) not in tasks):
        raise ViewError("selected task is not present in the current plan")
    actionable = envelope["plan"].get("actionable", [])
    if (not isinstance(actionable, list) or any(identifier(t) not in tasks for t in actionable)
            or len(set(actionable)) != len(actionable)):
        raise ViewError("invalid canonical actionable task set")
    contract = envelope["plan"].get("contract", {})
    if not isinstance(contract, dict):
        raise ViewError("invalid plan contract projection")
    cycles = _cycle_members(tasks)
    task_criteria = {}
    plan_id = envelope["plan"].get("plan_id")
    for criterion in criteria.values():
        if (criterion.get("source") == "plan-task" and criterion.get("plan_id") == plan_id
                and isinstance(plan_id, str) and PLAN_ID.fullmatch(plan_id)):
            task_criteria.setdefault(criterion.get("plan_task_id"), []).append(criterion)
    output = []
    for key, task in tasks.items():
        if task_id is not None and key != task_id:
            continue
        state = task.get("state", "unknown")
        blockers = []
        if contract.get("bound") is True and contract.get("satisfied") is False and state not in ("done", "cancelled"):
            blockers.append({"code": "plan-contract", "reason": text(contract.get("blocker") or "unknown", 160),
                             "actor": "parent", "source": "plan.contract"})
        for dependency in task.get("depends", []):
            other = tasks.get(dependency)
            if other is None or other.get("state") != "done":
                blockers.append({"code": "dependency", "task_id": dependency,
                                 "state": text(other.get("state", "unknown"), 80) if other else "missing",
                                 "actor": "unknown", "source": "plan.tasks.depends"})
        if key in cycles:
            blockers.append({"code": "dependency-cycle", "actor": "parent", "source": "plan.tasks.depends"})
        if state in ("claimed", "running"):
            blockers.append({"code": "active-attempt", "actor": "assigned-worker", "source": "plan.tasks.state"})
        elif state == "review":
            blockers.append({"code": "parent-review-or-admission", "actor": "parent", "source": "plan.tasks.state"})
        elif state == "landing":
            blockers.append({"code": "landing-transaction", "actor": "parent", "source": "plan.tasks.state"})
        elif state == "blocked" and not blockers:
            blockers.append({"code": "reason-not-recorded", "actor": "unknown", "source": "plan.tasks.state"})
        elif state == "ready" and key not in actionable and not blockers:
            blockers.append({"code": "not-actionable-reason-unrecorded", "actor": "unknown", "source": "plan.actionable"})
        evidence = [{"id": c["id"], "reported_status": c["status"]} for c in task_criteria.get(key, [])]
        output.append({"id": key, "title": text(task.get("title", "")), "state": text(state, 80),
                       "reported_actionable": key in actionable, "blockers": bounded(blockers, limit),
                       "criteria": bounded(evidence, limit)})
    actions = []
    for row in envelope.get("next_actions", []):
        actions.append({key: text(row.get(key, ""), 500 if key == "command" else 300)
                        for key in ("id", "authority", "reason", "command")})
    output.sort(key=lambda row: (not bool(row["blockers"]["total"]), row["id"]))
    return {"schema": 1, "kind": "oms-next-explanation", "state_digest": envelope["state_digest"],
            "authority": "read-projection-only", "tasks": bounded(output, limit),
            "canonical_next_actions": bounded(actions, limit), "action_scope": "whole-plan",
            "note": "Parent means the existing controller, not necessarily the person. Suggestions do not grant authority or run commands. Done/cancelled states are not new verification evidence."}
