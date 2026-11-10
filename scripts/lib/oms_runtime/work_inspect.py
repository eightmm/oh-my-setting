"""Pure, bounded work views over canonical TaskEnvelope projections.

These views never infer authority, run commands, or upgrade evidence. A saved
snapshot is historical data, not an authenticated receipt or an event stream.
"""
from __future__ import annotations

from collections import Counter
from datetime import datetime, timezone
import re
import unicodedata

MAX_ROWS = 4096
ID = re.compile(r"[A-Za-z0-9._:-]{1,160}\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")
STATES = {"verified", "failed", "missing", "stale", "inconclusive", "skipped_with_reason"}


class WorkViewError(ValueError):
    """Malformed snapshots are errors, never empty successful views."""


def _object(value, field):
    if not isinstance(value, dict):
        raise WorkViewError("invalid work snapshot: " + field)
    return value


def _rows(value, field):
    if not isinstance(value, list) or len(value) > MAX_ROWS:
        raise WorkViewError("invalid or oversized work snapshot: " + field)
    return value


def _text(value, field, maximum=4096):
    if not isinstance(value, str) or len(value) > maximum:
        raise WorkViewError("invalid work snapshot: " + field)
    return "".join(" " if c in "\t\r\n" else "?" if unicodedata.category(c)[0] in "CZ" and c != " "
                   else c for c in value)


def _id(value, field, optional=False):
    if optional and value in (None, ""):
        return ""
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise WorkViewError("invalid work snapshot: " + field)
    return value


def _digest(value, field, optional=False):
    if optional and value in (None, ""):
        return ""
    if not isinstance(value, str) or not DIGEST.fullmatch(value):
        raise WorkViewError("invalid work snapshot: " + field)
    return value


def _bool(value, field):
    if type(value) is not bool:
        raise WorkViewError("invalid work snapshot: " + field)
    return value


def _time(value):
    text = _text(value, "generated_at", 80)
    try:
        parsed = datetime.fromisoformat(text.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            raise ValueError
        return parsed.astimezone(timezone.utc)
    except ValueError:
        raise WorkViewError("work snapshot needs a timezone-aware generated_at") from None


def _limit(limit):
    if type(limit) is not int or not 1 <= limit <= 100:
        raise WorkViewError("limit must be an integer from 1 to 100")
    return limit


def _bounded(rows, limit):
    return {"total": len(rows), "items": rows[:limit], "omitted": max(0, len(rows) - limit)}


def _unique(rows, field):
    values = [r[field] for r in rows]
    if len(values) != len(set(values)):
        raise WorkViewError("work snapshot has duplicate " + field)


def _validate(raw):
    raw = _object(raw, "root")
    if type(raw.get("schema")) is not int or raw["schema"] != 2:
        raise WorkViewError("unsupported TaskEnvelope schema (expected 2)")
    _time(raw.get("generated_at"))
    state = _digest(raw.get("state_digest"), "state_digest")
    plan = _object(raw.get("plan"), "plan")
    present = _bool(plan.get("present"), "plan.present")
    pid = _id(plan.get("plan_id"), "plan_id", optional=True)
    task = _object(raw.get("task"), "task")
    tid = _id(task.get("task_id"), "active task id", optional=True)
    tasks = []
    for row in _rows(plan.get("tasks"), "tasks"):
        row = _object(row, "task row")
        depends = [_id(d, "dependency") for d in _rows(row.get("depends"), "depends")]
        if len(depends) != len(set(depends)):
            raise WorkViewError("work snapshot has duplicate dependency")
        tasks.append({"id": _id(row.get("id"), "task id"),
                      "title": _text(row.get("title"), "task title"),
                      "state": _text(row.get("state"), "task state", 80),
                      "depends": depends,
                      "verify_digest": _digest(row.get("verify_digest"), "verify digest", optional=True),
                      "verify_present": _bool(row.get("verify_present"), "verify_present")})
    _unique(tasks, "id")
    actionable = [_id(t, "actionable task") for t in _rows(plan.get("actionable", []), "actionable")]
    if len(actionable) != len(set(actionable)) or not set(actionable) <= {t["id"] for t in tasks}:
        raise WorkViewError("invalid work snapshot: actionable task set")
    if not present and (pid or tasks or actionable):
        raise WorkViewError("absent plan carries live task state")
    contract = _object(plan.get("contract", {}), "contract")
    contract_view = {}
    for key in ("bound", "satisfied"):
        if key in contract:
            contract_view[key] = _bool(contract[key], "contract." + key)
    contract_view["blocker"] = _text(contract.get("blocker", ""), "contract blocker", 300)
    criteria = []
    for row in _rows(raw.get("criteria"), "criteria"):
        row = _object(row, "criterion")
        status = row.get("status")
        if not isinstance(status, str) or status not in STATES:
            raise WorkViewError("invalid work snapshot: criterion status")
        evidence = []
        for item in _rows(row.get("evidence"), "evidence"):
            item = _object(item, "evidence row")
            evidence.append({key: _text(item.get(key, ""), "evidence " + key, 4096)
                             for key in ("evidence_ref", "kind", "status", "signal", "source", "support", "ts")})
        criteria.append({"id": _id(row.get("id"), "criterion id"),
                         "text": _text(row.get("text"), "criterion text"),
                         "source": _text(row.get("source"), "criterion source", 80),
                         "status": status,
                         "plan_task_id": _id(row.get("plan_task_id"), "plan task link", optional=True),
                         "plan_id": _id(row.get("plan_id"), "criterion plan id", optional=True),
                         "active_task_id": _id(row.get("active_task_id"), "active task link", optional=True),
                         "evidence": evidence})
    _unique(criteria, "id")
    actions = []
    for row in _rows(raw.get("next_actions"), "next_actions"):
        row = _object(row, "action")
        priority = row.get("priority")
        if type(priority) is not int or not 0 <= priority <= 1000:
            raise WorkViewError("invalid work snapshot: action priority")
        actions.append({"id": _id(row.get("id"), "action id"), "priority": priority,
                        "authority": _text(row.get("authority"), "action authority", 160),
                        "reason": _text(row.get("reason"), "action reason"),
                        "command": _text(row.get("command"), "action command")})
    _unique(actions, "id")
    sources = []
    for row in _rows(raw.get("sources"), "sources"):
        row = _object(row, "source")
        sources.append({"path": _text(row.get("path"), "source path"),
                        "sha256": _digest(row.get("sha256"), "source sha256", optional=True)})
    _unique(sources, "path")
    repo = _object(raw.get("repo"), "repo")
    head = repo.get("head")
    if head is not None and (not isinstance(head, str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", head)):
        raise WorkViewError("invalid work snapshot: head")
    return {"state_digest": state, "generated_at": raw["generated_at"], "head": head,
            "plan_id": pid, "active_task_id": tid, "tasks": tasks, "criteria": criteria,
            "actions": actions, "actionable": actionable, "contract": contract_view,
            "sources": sources}


def _base(kind, value):
    return {"schema": 1, "kind": kind, "generated_at": value["generated_at"],
            "state_digest": value["state_digest"], "plan_id": value["plan_id"] or None,
            "read_only": True, "authoritative": False,
            "note": "Read-time projection only. Commands are text, not execution or permission. Missing links are not inferred."}


def evidence_matrix(envelope, *, limit=20, task_id=None):
    """Criterion -> proven plan-task/active-task link -> existing evidence refs."""
    _limit(limit)
    value = _validate(envelope)
    tasks = {t["id"]: t for t in value["tasks"]}
    if task_id is not None and task_id not in tasks and task_id != value["active_task_id"]:
        raise WorkViewError("task is absent from this work snapshot")
    rows = []
    linked = set()
    for criterion in value["criteria"]:
        pid, tid = criterion["plan_id"], criterion["plan_task_id"]
        task = tasks.get(tid)
        if tid and task and pid and pid == value["plan_id"]:
            link = "plan-task-lineage"
            linked.add(tid)
        elif criterion["active_task_id"] and criterion["active_task_id"] == value["active_task_id"]:
            tid, task, link = criterion["active_task_id"], None, "active-task-lineage"
        else:
            tid, task, link = "", None, "unrecorded"
        if task_id is not None and tid != task_id:
            continue
        rows.append({"criterion_id": criterion["id"], "criterion": criterion["text"],
                     "status": criterion["status"], "task_id": tid or None, "link": link,
                     "task_state": task["state"] if task else None,
                     "verify_digest": (task["verify_digest"] or None) if task else None,
                     "evidence": _bounded(criterion["evidence"], limit)})
    return dict(_base("oms-evidence-matrix", value), criteria=_bounded(rows, limit),
                status_counts=dict(sorted(Counter(r["status"] for r in rows).items())),
                filter_task=task_id, total_declared_criteria=len(value["criteria"]),
                tasks_without_criterion_link=_bounded(sorted(set(tasks) - linked), limit),
                plan_acceptance_not_landing=True,
                limitation="No new covers mappings or landed-commit links are invented; existing evidence decides criterion status.")


def action_inbox(envelope, *, limit=20, task_id=None):
    """Explain direct dependencies and canonical next actions without a second scheduler."""
    _limit(limit)
    value = _validate(envelope)
    tasks = {t["id"]: t for t in value["tasks"]}
    if task_id is not None and task_id not in tasks:
        raise WorkViewError("task is absent from this work snapshot")
    contract = value["contract"]
    contract_blocked = contract.get("bound") is True and contract.get("satisfied") is False
    rows = []
    for tid, task in tasks.items():
        if task_id is not None and tid != task_id:
            continue
        waits = []
        for dependency in task["depends"]:
            state = tasks[dependency]["state"] if dependency in tasks else "unknown"
            if state != "done":
                waits.append({"task_id": dependency, "state": state,
                              "reason": "dependency-missing" if dependency not in tasks else "dependency-not-done"})
        reasons = []
        if contract_blocked:
            reasons.append({"code": "plan-contract-blocked", "detail": contract["blocker"] or "unknown"})
        if waits:
            reasons.append({"code": "dependencies-unfinished", "detail": "Cancelled tasks do not satisfy dependencies."})
        actionable = tid in value["actionable"]
        contradiction = actionable and bool(waits or contract_blocked or task["state"] != "ready")
        if contradiction:
            reasons.append({"code": "conflicting-projection", "detail": "Re-read canonical plan state before acting."})
        if not reasons:
            labels = {"ready": "ready" if actionable else "not-actionable-reason-unreported",
                      "claimed": "attempt-active", "running": "attempt-active",
                      "review": "parent-review-pending", "landing": "landing-in-progress",
                      "done": "recorded-done", "cancelled": "cancelled-not-verified"}
            reasons.append({"code": labels.get(task["state"], "reason-unreported"), "detail": "Recorded task state; not a completion receipt."})
        rows.append({"task_id": tid, "title": task["title"], "state": task["state"],
                     "canonical_actionable": actionable, "conflicting_projection": contradiction,
                     "depends_waiting": _bounded(waits, limit), "reasons": reasons,
                     "next_actor": "parent-controller", "human_decision_required": None})
    actions = [dict(a, next_actor="parent-controller", human_decision_required=None)
               for a in sorted(value["actions"], key=lambda a: (-a["priority"], a["id"]))]
    return dict(_base("oms-action-inbox", value), tasks=_bounded(rows, limit),
                actions=_bounded(actions, limit), filter_task=task_id,
                limitation="Authority labels do not identify a human. No inferred root cause, claim, retry, dispatch or approval occurs.")


def return_brief(current, before=None, *, limit=20):
    """Compare two snapshots; never claim to have observed intermediate events."""
    _limit(limit)
    now = _validate(current)
    result = dict(_base("oms-return-brief", now), baseline="absent", history_complete=False,
                  current=evidence_matrix(current, limit=limit), inbox=action_inbox(current, limit=limit),
                  limitation="Snapshot differences, not event history. Repeated states and intermediate failures cannot be reconstructed.")
    if before is None:
        return result
    old = _validate(before)
    reasons = []
    if _time(old["generated_at"]) > _time(now["generated_at"]):
        reasons.append("baseline-in-future")
    if old["plan_id"] or now["plan_id"]:
        if not old["plan_id"] or old["plan_id"] != now["plan_id"]:
            reasons.append("plan-lineage-changed")
    elif not old["active_task_id"] or old["active_task_id"] != now["active_task_id"]:
        reasons.append("task-lineage-unavailable-or-changed")
    if old["active_task_id"] != now["active_task_id"]:
        reasons.append("active-task-changed")
    old_sources = {s["path"]: s["sha256"] for s in old["sources"]}
    new_sources = {s["path"]: s["sha256"] for s in now["sources"]}
    changed_sources = [{"path": path, "before": old_sources.get(path), "after": new_sources.get(path)}
                       for path in sorted(old_sources.keys() | new_sources.keys())
                       if old_sources.get(path) != new_sources.get(path)]
    result.update(baseline="compared", baseline_at=old["generated_at"],
                  baseline_state_digest=old["state_digest"], recorded_scope_matches=not reasons,
                  mismatch_reasons=reasons, head_changed=old["head"] != now["head"],
                  source_changes=_bounded(changed_sources, limit),
                  same_snapshot_digest=old["state_digest"] == now["state_digest"])
    changes = {}
    for kind, key in (("tasks", "state"), ("criteria", "status")):
        left = {r["id"]: r for r in old[kind]}
        right = {r["id"]: r for r in now[kind]}
        updated = []
        if not reasons:
            for ident in sorted(left.keys() & right.keys()):
                changed_fields = [field for field in (key, "title", "text", "verify_digest", "evidence")
                                  if left[ident].get(field) != right[ident].get(field)]
                if changed_fields:
                    updated.append({"id": ident, "before": left[ident][key], "after": right[ident][key],
                                    "changed_fields": changed_fields})
        changes[kind] = {"transitions_suppressed": bool(reasons),
                         "added": _bounded(sorted(right.keys() - left.keys()), limit),
                         "removed": _bounded(sorted(left.keys() - right.keys()), limit),
                         "changed": _bounded(updated, limit)}
    result["changes"] = changes
    return result


def render(report):
    """Text view of our own validated projection; no command execution or I/O."""
    lines = [report["kind"], "read-only; no approval or verification is created", report["note"]]
    kind = report["kind"]
    if kind == "oms-evidence-matrix":
        rows = report["criteria"]
        lines.append("criteria: %d shown / %d; %d omitted" % (len(rows["items"]), rows["total"], rows["omitted"]))
        for row in rows["items"]:
            lines.append("%s [%s] task=%s link=%s" % (row["criterion_id"], row["status"], row["task_id"] or "unknown", row["link"]))
            lines.append("  " + row["criterion"])
            for evidence in row["evidence"]["items"]:
                lines.append("  evidence: %s [%s] %s" % (evidence["evidence_ref"] or "unrecorded", evidence["status"], evidence["source"]))
            if row["evidence"]["omitted"]:
                lines.append("  evidence omitted: %d" % row["evidence"]["omitted"])
        absent = report["tasks_without_criterion_link"]
        lines.append("tasks without recorded link: %d (%d shown)" % (absent["total"], len(absent["items"])))
        lines.extend("  " + t for t in absent["items"])
    elif kind == "oms-action-inbox":
        for row in report["tasks"]["items"]:
            lines.append("%s [%s]: %s" % (row["task_id"], row["state"], row["title"]))
            lines.extend("  " + r["code"] + ": " + r["detail"] for r in row["reasons"])
            lines.extend("  waits: %s [%s]" % (d["task_id"], d["state"]) for d in row["depends_waiting"]["items"])
            if row["depends_waiting"]["omitted"]:
                lines.append("  waiting dependencies omitted: %d" % row["depends_waiting"]["omitted"])
        lines.append("tasks omitted: %d" % report["tasks"]["omitted"])
        lines.append("Canonical suggested actions (not executed):")
        lines.extend("  %s [%s]: %s -> %s" % (a["id"], a["authority"], a["reason"], a["command"]) for a in report["actions"]["items"])
        lines.append("actions omitted: %d" % report["actions"]["omitted"])
    elif kind == "oms-return-brief":
        lines.append("baseline: " + report["baseline"] + "; event history: incomplete")
        if report["baseline"] != "absent":
            lines.append("scope: " + ("same recorded lineage" if report["recorded_scope_matches"] else ", ".join(report["mismatch_reasons"])))
            for label, changes in report["changes"].items():
                lines.append(label + (" (transitions suppressed)" if changes["transitions_suppressed"] else ""))
                for direction in ("added", "removed"):
                    rows = changes[direction]
                    lines.append("  %s: %s; %d omitted" % (direction, ", ".join(rows["items"]) or "none", rows["omitted"]))
                for row in changes["changed"]["items"]:
                    lines.append("  %s: %s -> %s (%s)" % (row["id"], row["before"], row["after"], ", ".join(row["changed_fields"])))
                lines.append("  changed omitted: %d" % changes["changed"]["omitted"])
            for row in report["source_changes"]["items"]:
                lines.append("source changed: " + row["path"])
            lines.append("source changes omitted: %d" % report["source_changes"]["omitted"])
        lines += ["", render(report["current"]), "", render(report["inbox"])]
    else:
        raise WorkViewError("unsupported work report kind")
    lines.append(report.get("limitation", ""))
    return "\n".join(lines) + "\n"
