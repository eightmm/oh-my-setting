"""Read-only collaboration dashboard projected from collected OMS query output.

Input is one collection directory written by scripts/dashboard.sh: for each
source NAME (state, artifacts, attempts) a NAME.json payload, NAME.rc exit
code, and NAME.err stderr. Nothing here runs a model or touches the network;
the only .oms file read is the repository plan, for per-task claimants. Every label is untrusted: control, format, and bidi
characters are replaced before anything reaches a terminal or JSON consumer.
"""

import argparse
import datetime
import json
import math
import os
import re
import shlex
import sys
import unicodedata
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from inbox_projection import (COMMAND_HIDDEN_NOTICE, has_invisible_characters,
                              project_inbox)  # noqa: E402
from thread_live import DURABLE, _load, safe_path  # noqa: E402

PLAN_READER = _load("plan-retire.py")["bounded_regular"]

SCHEMA = 1
MAX_ROWS = 8
MAX_LABEL = 160
MAX_SOURCE_BYTES = 4 * 1024 * 1024
COVERAGE_NOTE = "OMS-managed records only; native provider subagents are not observed"
FALLBACK_REASONS = {"capacity", "capacity-no-fallback", "capacity-dirty-worktree",
                    "model-unavailable", "policy-declined", "model-safeguard",
                    "provider-model-unavailable", "provider-capacity", "provider-down"}
MODEL_CLASSES = {"explicit", "provider-default", "role-default", "fast", "balanced", "deep"}
ATTRIBUTIONS = {"transport", "configured-default", "ambiguous", "unknown"}
ARTIFACT_STATUSES = {"success", "unresolved", "resolved"}
CRITERION_ORDER = ("failed", "stale", "missing", "inconclusive", "skipped_with_reason", "verified")


def clean(value, limit=MAX_LABEL):
    """Bounded single-line text with every control/format character neutralized."""
    if value is None:
        return None
    text = "".join(" " if ch in "\t\n\r" or unicodedata.category(ch)[0] == "Z"
                   else "?" if unicodedata.category(ch)[0] == "C" else ch
                   for ch in str(value) if ch not in "\ufe0e\ufe0f")
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit - 3] + "..."


def clean_command(value):
    """Requote shell words without exposing terminal controls or changing bytes."""
    if value is None:
        return None
    original = str(value)
    if has_invisible_characters(original):
        return original

    def unsafe(char):
        return unicodedata.category(char)[0] == "C" or (
            ord(char) > 127 and (char.isspace() or unicodedata.category(char)[0] == "Z"))

    def quote_argument(argument):
        argument = str(argument)
        if not any(unsafe(char) for char in argument):
            return shlex.quote(argument)
        escaped = []
        for char in argument:
            if unsafe(char):
                escaped.extend("\\x%02x" % byte for byte in char.encode("utf-8", "surrogateescape"))
            elif char == "\\":
                escaped.append("\\\\")
            elif char == "'":
                escaped.append("\\'")
            else:
                escaped.append(char)
        return "$'" + "".join(escaped) + "'"

    lexer = shlex.shlex(original, posix=True, punctuation_chars=";&|")
    lexer.whitespace_split = True
    lexer.commenters = ""
    tokens = list(lexer)
    output = []
    operators = {";", "&&", "||", "|", "&"}
    for token in tokens:
        if token in operators:
            output.append(token)
            continue
        assignment = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$", token)
        if assignment:
            output.append(assignment.group(1) + "=" + quote_argument(assignment.group(2)))
        else:
            output.append(quote_argument(token))
    return " ".join(output)


def count(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        return None
    try:
        if not math.isfinite(value):
            return None
    except OverflowError:
        return None
    return value


def mapping(value):
    return value if isinstance(value, dict) else {}


def listing(value):
    return value if isinstance(value, list) else []


def enum(value, allowed):
    return value if isinstance(value, str) and value in allowed else None


def counts(value):
    return {clean(key, 40): n for key, n in sorted(mapping(value).items()) if count(n) is not None}


def load_source(directory, name, repo_path=""):
    """Return (payload, error) for one collected source; error is None on success."""
    base = Path(directory)
    try:
        rc = int((base / (name + ".rc")).read_text(encoding="utf-8").strip() or "1")
    except (OSError, ValueError):
        rc = 1
    if rc != 0:
        # Collector stderr may quote private record contents or credentials.
        return None, "exit %d" % rc
    try:
        with open(base / (name + ".json"), "rb") as handle:
            raw = handle.read(MAX_SOURCE_BYTES + 1)
        if len(raw) > MAX_SOURCE_BYTES:
            return None, "source exceeds 4 MiB display limit"
        return json.loads(raw), None
    except (OSError, ValueError, RecursionError):
        return None, "returned invalid JSON"


def panel_metadata(row):
    refs = mapping(row.get("refs"))
    if refs.get("panel_role") not in ("main", "worker", "advisor", "reviewer", "researcher"):
        return {}
    return {"panel": {key: clean(refs.get("panel_" + key), 160) or None for key in (
        "role", "owner", "harness", "purpose", "workload", "provider", "model", "effort", "fallback", "location", "access", "label", "room_id", "room_participant", "lease_id"
    )}, "parent_attempt_id": clean(row.get("parent_attempt_id")) or None}


def project_attempts(state, rows, error):
    ops = mapping(state.get("agent_operations")) if state is not None else {}
    block = {
        "available": state is not None and ops.get("healthy") is True and error is None,
        "total": count(ops.get("total")),
        "active": count(ops.get("active")),
        "by_state": counts(ops.get("by_state")),
        "note": "a done attempt ended its work; acceptance is judged by evidence",
        "recent": [],
        "active_recent": [],
    }
    if not block["available"]:
        block["error"] = ("lifecycle stream invalid" if ops.get("healthy") is False
                          else error or "state unavailable")
        return block
    for row in reversed(listing(ops.get("active_latest"))[-MAX_ROWS:]):
        if isinstance(row, dict):
            projected = {key: clean(row.get(key), 160) or None for key in (
                "attempt_id", "state", "provider", "tool", "task_id", "reason_code", "updated_at"
            )}
            projected.update(panel_metadata(row))
            block["active_recent"].append(projected)
    for row in reversed(listing(rows)[-MAX_ROWS:]):
        if not isinstance(row, dict):
            continue
        usage, reports = mapping(row.get("usage")), mapping(row.get("usage_reports"))

        def reported(key):
            return count(usage.get(key)) if count(reports.get(key)) else None
        block["recent"].append({
            "attempt_id": clean(row.get("attempt_id")),
            "state": clean(row.get("state"), 40),
            "provider": clean(row.get("provider"), 40) or None,
            "tool": clean(row.get("tool"), 60) or None,
            "task_id": clean(row.get("task_id")) or None,
            "reason_code": clean(row.get("reason_code"), 64) or None,
            "updated_at": clean(row.get("updated_at"), 40),
            "tokens": reported("tokens"),
            "cost_microusd": reported("cost_microusd"),
            **panel_metadata(row),
        })
    return block


def project_usage(value):
    usage = mapping(value)
    if not usage:
        return None
    return {**{key: count(usage.get(key)) for key in (
        "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens")},
        "cache_in_input": usage.get("cache_in_input") if isinstance(usage.get("cache_in_input"), bool) else None,
        "session_cumulative": usage.get("session_cumulative") is True}


def project_operations(rows, error):
    block = {"available": error is None, "recent": [], "omitted": 0}
    reviews = {"outcomes": 0, "passed": 0, "failed": 0, "unknown": 0, "seat_answers": 0}
    if error is not None:
        block["error"] = error
        return block, None
    outcomes = [row for row in listing(rows)
                if isinstance(row, dict) and row.get("kind") != "artifact-resolution"]
    for row in outcomes:
        exit_code = count(row.get("exit"))
        if row.get("kind") == "review-outcome":
            reviews["outcomes"] += 1
            reviews["unknown" if exit_code is None else "passed" if exit_code == 0 else "failed"] += 1
        elif row.get("kind") == "review":
            reviews["seat_answers"] += 1
    block["omitted"] = max(0, len(outcomes) - MAX_ROWS)
    for row in reversed(outcomes[-MAX_ROWS:]):
        status = row.get("status")
        exit_code = count(row.get("exit"))
        if enum(status, ARTIFACT_STATUSES) is None:
            status = "unknown" if exit_code is None else "success" if exit_code == 0 else "unresolved"
        reason = row.get("fallback_reason")
        attribution = row.get("model_attribution")
        block["recent"].append({
            "event_id": clean(row.get("event_id")),
            "ts": clean(row.get("ts"), 40),
            "kind": clean(row.get("kind"), 40),
            "provider": clean(row.get("provider"), 40) or None,
            "task_id": clean(row.get("task_id")) or None,
            "attempt_id": clean(row.get("attempt_id")) or None,
            "exit": exit_code,
            "status": status,
            "verify_exit": count(row.get("verify_exit")),
            "route_class": enum(row.get("model_class"), MODEL_CLASSES),
            "requested_model": clean(row.get("requested_model"), 80) or None,
            "selected_model": clean(row.get("selected_model"), 80) or None,
            "served_model": clean(row.get("served_model"), 80) or None,
            "model_attribution": enum(attribution, ATTRIBUTIONS) or "unknown",
            "fallback_used": row.get("fallback_used") is True,
            "fallback_reason": (enum(reason, FALLBACK_REASONS) or "other") if reason else None,
            "tokens": count(row.get("tokens")),
            "provider_usage": project_usage(row.get("provider_usage")),
            "cost_usd": number(row.get("cost_usd")),
        })
    return block, reviews


def project_acceptance(runtime):
    if runtime.get("healthy") is not True:
        return {"available": False, "error": "runtime projection unavailable"}
    evidence = mapping(runtime.get("evidence"))
    criteria = [item for item in listing(runtime.get("criteria")) if isinstance(item, dict)]
    rank = {name: index for index, name in enumerate(CRITERION_ORDER)}
    ordered = sorted(criteria, key=lambda item: rank.get(enum(item.get("status"), rank), len(rank)))
    return {
        "available": True,
        "total": len(criteria),
        "counts": counts(evidence.get("counts")),
        "complete": evidence.get("complete") is True,
        "criteria": [{
            "id": clean(item.get("id")),
            "status": clean(item.get("status"), 40) or "unknown",
            "source": clean(item.get("source"), 40),
            "text": clean(item.get("text")),
        } for item in ordered[:MAX_ROWS]],
        "omitted": max(0, len(criteria) - MAX_ROWS),
        "note": "worker completion is not acceptance",
    }


def read_plan(repo_path):
    """Read a bounded repo-local plan without following links or blocking on special files."""
    if not repo_path:
        return {}, []
    try:
        path = safe_path(repo_path, ".oms/plan/tasks.json")
        if not path.exists():
            return {}, []
        ancestry = DURABLE["component_snapshot"](str(repo_path), str(path), "plan")
        raw, _ = PLAN_READER(str(path), "plan", MAX_SOURCE_BYTES)
        DURABLE["components_unchanged"](ancestry, "plan")
        data = json.loads(raw)
        if not isinstance(data, dict):
            return {}, []
        rows = data.get("tasks", {})
        if not isinstance(rows, (dict, list)):
            return {}, []
    except (OSError, ValueError, RecursionError, SystemExit):
        return {}, []
    rows = list(rows.values()) if isinstance(rows, dict) else rows if isinstance(rows, list) else []
    tasks = []
    for row in rows:
        if not isinstance(row, dict) or not row.get("id"):
            continue
        depends = row.get("depends")
        if (not isinstance(row["id"], str) or not re.fullmatch(r"[A-Za-z0-9._-]+", row["id"])
                or depends is not None and (not isinstance(depends, list)
                    or any(not isinstance(item, str) for item in depends))):
            return {}, []
        tasks.append(row)
    return data, tasks


def plan_tasks(repo_path):
    """Bounded id/title/state/lease/claimant rows of the repository plan; empty when it cannot be read."""
    _, rows = read_plan(repo_path)
    return [{"id": clean(row.get("id"), 40), "title": clean(row.get("title"), 80),
             "state": clean(row.get("state"), 20),
             "lease_id": clean(row.get("lease_id"), 80) or None,
             **({"completion_kind": clean(row["completion_kind"], 40)}
                if isinstance(row.get("completion_kind"), str) else {}),
             "claimed_by": clean(row.get("claimed_by_participant"), 80) or None,
             **({"claimed_at": clean(row["claimed_at"], 40)}
                if isinstance(row.get("claimed_at"), str) else {})}
            for row in rows]


def build(directory, repo_name, repo_path="", now=None):
    state, state_error = load_source(directory, "state", repo_path)
    if state is not None and (not isinstance(state, dict) or state.get("schema") != 1):
        state, state_error = None, "unsupported state contract"
    artifacts, artifacts_error = load_source(directory, "artifacts", repo_path)
    if artifacts is not None and (not isinstance(artifacts, dict) or artifacts.get("schema") != 1
                                  or not isinstance(artifacts.get("rows"), list)):
        artifacts, artifacts_error = None, "unsupported artifact-index contract"
    attempts, attempts_error = load_source(directory, "attempts", repo_path)
    if attempts is not None and not isinstance(attempts, list):
        attempts, attempts_error = None, "unsupported lifecycle contract"
    sources = {"state": state_error or "ok", "artifacts": artifacts_error or "ok",
               "attempts": attempts_error or "ok"}
    stamp = now or datetime.datetime.now(datetime.timezone.utc)
    report = {
        "schema": SCHEMA,
        "kind": "oms-dashboard",
        "generated_at": stamp.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "repo": {"name": clean(repo_name, 80) or None, "branch": None, "head": None},
        "coverage": COVERAGE_NOTE,
        "collection": {"ok": all(value == "ok" for value in sources.values()), "sources": sources},
    }
    operations, reviews = project_operations(
        mapping(artifacts).get("rows") if artifacts is not None else None, artifacts_error)
    report["operations"] = operations
    report["reviews"] = reviews
    if state is None:
        report["attempts"] = project_attempts(None, None, attempts_error)
        for name in ("goal", "scope", "task", "plan", "delegations", "acceptance", "attention"):
            report[name] = None
        return report
    ci, runtime = mapping(state.get("ci")), mapping(state.get("runtime"))
    report["repo"]["branch"] = clean(ci.get("current_branch"), 80)
    sha = ci.get("current_sha")
    report["repo"]["head"] = sha[:12] if isinstance(sha, str) and re.fullmatch(r"[0-9a-f]{7,64}", sha) else None
    task, plan = mapping(state.get("task")), mapping(state.get("plan"))
    objective = mapping(runtime.get("objective"))
    goal_text = objective.get("text") if objective.get("text") else task.get("goal") or plan.get("goal")
    goal_source = objective.get("source") if objective.get("text") else (
        "task" if task.get("goal") else "plan" if plan.get("goal") else None)
    report["goal"] = {"text": clean(goal_text, 240), "source": clean(goal_source, 20)} if goal_text else None
    scope = mapping(runtime.get("scope"))
    report["scope"] = {
        "allowed": [clean(path, 80) for path in listing(scope.get("allowed"))[:6]],
        "forbidden": [clean(path, 80) for path in listing(scope.get("forbidden"))[:6]],
        "source": clean(scope.get("allowed_source"), 20) or "unbounded",
    } if runtime.get("healthy") is True else None
    report["task"] = {
        "present": task.get("present") is True,
        "healthy": task.get("healthy") is not False,
        "task_id": clean(task.get("task_id")) or None,
        "status": clean(task.get("status"), 40) or None,
        "verification": clean(task.get("verification"), 40) or None,
        "stale": task.get("stale") is True,
        "next": clean(task.get("next"), 200) or None,
    }
    contract = mapping(plan.get("contract"))
    report["plan"] = {
        "present": plan.get("present") is True,
        "healthy": plan.get("healthy") is not False,
        "task_count": count(plan.get("task_count")),
        "by_state": counts(plan.get("by_state")),
        "actionable": [clean(item, 80) for item in listing(plan.get("actionable"))[:5]],
        "stale_claims": len(listing(plan.get("stale"))),
        "stale_reviews": len(listing(plan.get("stale_review"))),
        "idle_days": count(plan.get("idle_days")),
        "goal": clean(plan.get("goal"), 200) or None,
        "tasks": plan_tasks(repo_path) if plan.get("present") is True else [],
        "contract_blocker": (clean(contract.get("blocker") or "unknown", 80)
                             if contract.get("bound") and not contract.get("satisfied") else None),
    }
    report["attempts"] = project_attempts(state, attempts, attempts_error)
    report["delegations"] = [{
        "id": clean(row.get("id")),
        "provider": clean(row.get("provider"), 40) or None,
        "role": clean(row.get("role"), 40) or None,
        "task_id": clean(row.get("task_id")) or None,
        "requested_model": clean(row.get("model"), 80) or None,
        "reasoning_effort": clean(row.get("reasoning_effort"), 40) or None,
        "location": clean(row.get("location"), 160) or None,
        "live": row.get("live") is True,
        "started_at": clean(row.get("started_at"), 40),
    } for row in sorted((row for row in listing(state.get("delegations")) if isinstance(row, dict)),
                        key=lambda row: row.get("live") is not True)[:5]]
    report["acceptance"] = project_acceptance(runtime)
    inbox = project_inbox(state, include_threads=os.environ.get("OMS_THREAD_ATTENTION") != "0",
                          repo_path=repo_path or os.getcwd())
    report["attention"] = {
        "actionable": inbox["actionable"],
        "items": [{key: clean_command(item.get(key)) if key == "command" else clean(item.get(key), 200)
                   for key in ("priority", "code", "summary", "command")}
                  for item in inbox["items"][:MAX_ROWS]],
        "omitted": max(0, len(inbox["items"]) - MAX_ROWS),
        "next": [{key: clean_command(action.get(key)) if key == "command" else clean(action.get(key), 200)
                  for key in ("id", "authority", "command")}
                 for action in inbox["recommended_actions"][:1]],
    }
    return report


def char_width(ch):
    if unicodedata.category(ch) in ("Mn", "Me") or "\u1160" <= ch <= "\u11ff":
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


def display_width(text):
    return sum(char_width(ch) for ch in text)


ELLIPSIS = ["..."]


def use_unicode(flag):
    """Board renders truncate with one-column "…"; ASCII output and CLI snapshots keep "..."."""
    ELLIPSIS[0] = "…" if flag else "..."


def fit(text, width):
    if display_width(text) <= width:
        return text
    mark = ELLIPSIS[0][:max(0, width)]
    out, used = [], 0
    for ch in text:
        step = char_width(ch)
        if used + step > width - len(mark):
            break
        out.append(ch)
        used += step
    return "".join(out) + mark


def unknown(value, label="unknown"):
    return label if value is None else str(value)


def wrap_label(text, width, limit, max_chars=400):
    """Wrap labels by terminal cells, including Korean and unbroken symbols."""
    text, lines = clean(text, max_chars), []
    width = max(1, width)
    while text and len(lines) < limit:
        end, used = 0, 0
        for char in text:
            if used + display_width(char) > width:
                break
            used += display_width(char)
            end += 1
        if not end:
            return lines + [""]
        if end < len(text) and " " in text[:end]:
            end = text.rfind(" ", 0, end) or end
        lines.append(text[:end].rstrip())
        text = text[end:].lstrip()
    if text:
        lines[-1] = fit(lines[-1] + " " + text, width) if width >= 3 else lines[-1]
    return lines or [""]


def render(report, width, interval=None, height=None):
    lines = []
    command_groups = set()

    def add(text, priority=0, clip=True, group=None):
        lines.append((text, priority, clip, group))

    def add_command(prefix, command, priority=0, group=None):
        """Print a complete command verbatim after its separate label."""
        group = group if group is not None else object()
        command_groups.add(group)
        add(prefix.rstrip(), priority, clip=False, group=group)
        if has_invisible_characters(command):
            command = COMMAND_HIDDEN_NOTICE
        add(command, priority, clip=False, group=group)
    repo = report["repo"]
    head = "%s@%s" % (repo["branch"] or "?", repo["head"] or "?")
    refresh = "  refresh=%ss" % interval if interval else ""
    add("OMS dashboard  repo=%s  %s  snapshot=%s%s" % (repo["name"] or "?", head, report["generated_at"], refresh))
    add("coverage: " + report["coverage"])
    for name, status in sorted(report["collection"]["sources"].items()):
        if status != "ok":
            add("COLLECTION FAILED %s: %s" % (name, status))
    if report.get("task") is None:
        add("state: UNAVAILABLE - goal, plan, acceptance and attention cannot be shown")
    else:
        goal = report["goal"]
        add("goal: %s" % ("%s (%s)" % (goal["text"], goal["source"]) if goal else "none recorded"))
        scope = report["scope"]
        if scope is None:
            add("scope: unavailable (runtime projection)")
        elif scope["allowed"] or scope["forbidden"]:
            add("scope: allowed=%s (%s)  forbidden=%s" % (
                ",".join(scope["allowed"]) or "-", scope["source"], ",".join(scope["forbidden"]) or "-"))
        else:
            add("scope: unbounded (none recorded)")
        task = report["task"]
        if not task["healthy"]:
            add("task: UNAVAILABLE or invalid (oms agent-task status --json)")
        elif task["present"]:
            add("task: %s status=%s verification=%s%s" % (
                unknown(task["task_id"]), unknown(task["status"]), unknown(task["verification"]),
                "  STALE" if task["stale"] else ""))
        else:
            add("task: none active")
        plan = report["plan"]
        if not plan["healthy"]:
            add("plan: UNAVAILABLE or invalid (oms agent-plan status --json)")
        elif plan["present"]:
            add("plan: %s task(s)  %s%s%s" % (
                unknown(plan["task_count"]),
                " ".join("%s=%d" % item for item in plan["by_state"].items()) or "-",
                "  actionable=" + ",".join(plan["actionable"]) if plan["actionable"] else "",
                "  STALE claims=%d" % plan["stale_claims"] if plan["stale_claims"] else ""))
            if plan["contract_blocker"]:
                add("  PROJECT contract blocked: " + plan["contract_blocker"])
        else:
            add("plan: none")
    attempts = report["attempts"]
    if not attempts["available"]:
        add("attempts: UNAVAILABLE (%s)" % attempts.get("error", "unknown"))
    else:
        add("attempts (OMS lifecycle): %s total, %s active  %s" % (
            unknown(attempts["total"]), unknown(attempts["active"]),
            " ".join("%s=%d" % item for item in attempts["by_state"].items()) or "-"))
        recent_ids = {row["attempt_id"] for row in attempts["recent"]}
        visible = [row for row in attempts.get("active_recent", []) if row["attempt_id"] not in recent_ids]
        for row in visible + attempts["recent"]:
            add("  %s %s %s/%s task=%s tokens=%s cost_microusd=%s %s" % (
                row["attempt_id"], row["state"], row["provider"] or "unknown", row["tool"] or "?",
                row["task_id"] or "-", unknown(row.get("tokens")), unknown(row.get("cost_microusd")),
                row["updated_at"]), 1 if row["state"] in (
                    "working", "blocked", "waiting_input", "waiting_approval") else 2)
            panel = mapping(row.get("panel"))
            if panel:
                add("    role=%s owner=%s parent=%s model=%s effort=%s" % (
                    panel.get("role"), panel.get("owner"), row.get("parent_attempt_id") or "unattached",
                    panel.get("model") or "unrecorded", panel.get("effort") or "auto"), 1)
                add("    at=%s access=%s purpose=%s" % (
                    panel.get("location") or "unrecorded", panel.get("access") or "native",
                    panel.get("purpose") or "unrecorded"), 1)
                for line in wrap_label("Task: " + (panel.get("label") or "unrecorded"), width - 4, 8):
                    add("    " + line, 1)
    for row in report.get("delegations") or []:
        add("delegation: %s %s%s started=%s %s" % (
            row["id"], row["provider"] or "unknown", " role=" + row["role"] if row["role"] else "",
            row["started_at"] or "?", "live" if row["live"] else "ORPHAN (dead pid)"), 2)
    operations = report["operations"]
    if not operations["available"]:
        add("operations: UNAVAILABLE (%s)" % operations.get("error", "unknown"))
    else:
        add("operations (artifact index, newest first):" if operations["recent"]
            else "operations (artifact index): none recorded")
        for row in operations["recent"]:
            route = row["route_class"] or "unrecorded"
            if row["fallback_used"] or row["fallback_reason"]:
                route += " fallback=%s" % (row["fallback_reason"] or "yes")
            add("  %s %s%s model=%s route=%s exit=%s %s served=%s tokens=%s at=%s" % (
                fit(row["provider"] or "unknown", 12), fit(row["kind"] or "unknown", 16),
                " task=" + row["task_id"] if row["task_id"] else "",
                fit(unknown(row["selected_model"]), 28), route, unknown(row["exit"]),
                row["status"], unknown(row["served_model"], "unreported"),
                unknown(row["tokens"]), row["ts"]),
                1 if row["selected_model"] else 2)
        if operations["omitted"]:
            add("  ... %d older operation(s) in window" % operations["omitted"])
        reviews = report["reviews"]
        add("review gate: %s%s" % (
            "recorded window pass=%d fail=%d unknown=%d" % (
                reviews["passed"], reviews["failed"], reviews["unknown"]) if reviews["outcomes"]
            else "none recorded in window (not a pass)",
            "  seat answers=%d" % reviews["seat_answers"] if reviews["seat_answers"] else ""))
    acceptance = report.get("acceptance")
    if acceptance is not None:
        if not acceptance["available"]:
            add("acceptance: UNAVAILABLE (oms runtime doctor --strict)")
        elif not acceptance["total"]:
            add("acceptance: no criteria recorded; completion cannot be proven")
        else:
            add("acceptance: %d/%d verified  %s  (%s)" % (
                acceptance["counts"].get("verified", 0), acceptance["total"],
                " ".join("%s=%d" % item for item in acceptance["counts"].items()),
                acceptance["note"]))
            for item in acceptance["criteria"]:
                add("  %s %s [%s] %s" % (item["status"], item["id"], item["source"], item["text"]),
                    1 if item["status"] in ("failed", "stale") else 2)
            if acceptance["omitted"]:
                add("  ... %d more criterion/criteria" % acceptance["omitted"])
    attention = report.get("attention")
    if attention is not None:
        add("attention: %d item(s)" % attention["actionable"])
        for index, item in enumerate(attention["items"]):
            priority = 1 if index == 0 else 2
            group = object()
            add("  %s %s: %s" % (item["priority"], item["code"], item["summary"]),
                priority, group=group)
            add_command("     next: ", item["command"], 3, group=group)
        if attention["omitted"]:
            add("  ... %d more (oms inbox)" % attention["omitted"])
        for action in attention["next"]:
            add_command("next decision: %s (%s): " % (action["id"], action["authority"]),
                        action["command"])
    terminal_width = max(1, width)
    def row_cost(line):
        if line[2]:
            return 1
        return max(1, (display_width(line[0]) + terminal_width - 1) // terminal_width)

    if height and sum(row_cost(line) for line in lines) > height:
        groups = {}
        for index, line in enumerate(lines):
            key = line[3] if line[3] is not None else ("line", index)
            entry = groups.setdefault(key, {"indices": [], "cost": 0, "priority": line[1]})
            entry["indices"].append(index)
            entry["cost"] += row_cost(line)
            entry["priority"] = min(entry["priority"], line[1])
        indicator_rows = 1
        selected, remaining = set(), max(0, height - indicator_rows)
        for key, entry in sorted(groups.items(), key=lambda item: (item[1]["priority"], item[1]["indices"][0])):
            if entry["cost"] > remaining:
                continue
            selected.add(key)
            remaining -= entry["cost"]
        omitted = len(lines) - sum(len(groups[key]["indices"]) for key in selected)
        omitted_commands = sum(1 for key in groups if key in command_groups and key not in selected)
        if omitted_commands:
            suffix = "" if omitted_commands == 1 else "s"
            concise = "... %d command%s omitted for space; see oms inbox --json" % (
                omitted_commands, suffix)
            detailed = "... %d command%s omitted for space; %d detail line(s) hidden; see oms inbox --json" % (
                omitted_commands, suffix, omitted)
            indicator = detailed if display_width(detailed) <= width else concise
            indicator_line = (indicator, 0, True, None)
        else:
            indicator = "... %d detail line(s) hidden; single snapshot or --json shows more" % omitted
            indicator_line = (indicator, 0, True, None)
        lines = [line for index, line in enumerate(lines)
                 if (line[3] if line[3] is not None else ("line", index)) in selected]
        lines.append(indicator_line)
    return "\n".join(line if not clip or display_width(line) <= width else fit(line, width)
                     for line, _, clip, _ in lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory")
    parser.add_argument("--repo-name", default="")
    parser.add_argument("--repo-path", default="")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--width", type=int, default=100)
    parser.add_argument("--interval", type=int, default=0)
    parser.add_argument("--height", type=int, default=0)
    args = parser.parse_args(argv)
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(errors="replace")
    report = build(args.directory, args.repo_name, args.repo_path)
    if args.json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    else:
        # Leave the last terminal row for print's newline to avoid scrolling.
        print(render(report, max(20, min(args.width, 240)), args.interval or None,
                     max(1, min(args.height, 200) - 1) if args.height else None))
    return 0 if report["collection"]["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
