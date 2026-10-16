"""Owner-authorized lifecycle closure for stopped review workers."""
import argparse
import datetime as dt
import hashlib
import json
import os
import subprocess
import sys

import panel_results as saved
from dashboard_projection import clean, read_plan

POLICY = {"completed": ("done", "owner_completed"), "accepted": ("done", "owner_accepted"),
          "rejected": ("cancelled", "owner_rejected"), "failed": (None, "rework")}


def _main(row):
    return (row.get("tool") == "panel-main" and row.get("refs", {}).get("panel_role") == "main"
            and row.get("provider") in ("codex", "claude")
            and row.get("refs", {}).get("panel_owner", row.get("provider")) == row.get("provider"))


def _parents(repo, attempts, owner=None, main_id=None, require_dead=False):
    import terminal_panel as panel
    if main_id:
        main = next((a for a in attempts if a.get("attempt_id") == main_id), {})
        if not _main(main) or (owner and main.get("provider") != owner):
            raise ValueError("review settlement needs a known owning main")
        if require_dead and main.get("terminal") is not True:
            raise ValueError("--owner-attempt must name a dead main")
        if require_dead:
            member = main["refs"].get("panel_room_participant", main_id)
            if not member:
                raise ValueError("review settlement ownership is unknown")
            history = [a for a in attempts if (a.get("tool") == "panel-main"
                or a.get("refs", {}).get("panel_role") == "main") and a.get("refs", {}).get(
                "panel_room_participant", a.get("attempt_id")) == member]
            if not all(_main(row) for row in history):
                raise ValueError("review settlement ownership is unknown")
            import panel_release
            for row in history:
                panel_release.prove_host_absent(row.get("refs", {}), row["attempt_id"])
                panel_release.prove_native_absent(row)
            # The continuation helper requires one live main. Recovery instead
            # owns only this participant's chain, with every process proven gone.
            return {row["attempt_id"] for row in history}
        parents = panel.lineage_ids_from_rows(attempts, main["provider"],
            main.get("refs", {}).get("panel_room_participant", main_id), main_id)
        if not parents:
            raise ValueError("review settlement ownership is unknown")
        return parents
    current = os.environ.get("OMS_PANEL_MAIN_ATTEMPT")
    if not current:
        raise ValueError("review settlement ownership is unknown")
    bound = next((a for a in attempts if a.get("attempt_id") == current), {})
    parents = panel.owning_attempt(repo, owner or bound.get("provider") or panel.caller_owner(), None, os.environ)
    if not parents:
        raise ValueError("review settlement ownership is unknown")
    return parents


def _age(stamp):
    try:
        then = dt.datetime.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
        return max(0, int((dt.datetime.now(dt.timezone.utc) - then).total_seconds()))
    except (TypeError, ValueError):
        return None


def _stopped(repo, attempt, attempts, artifacts):
    refs = attempt.get("refs", {})
    member, room_id = refs.get("panel_room_participant"), refs.get("panel_room_id")
    if member or room_id:
        if not (member and room_id):
            return False
        import terminal_panel as panel
        parent = next((a for a in attempts if a.get("attempt_id") == attempt.get("parent_attempt_id")), {})
        return panel._dispatch_call_finished(repo, room_id, {"participant": member,
            "parent": parent.get("refs", {}).get("panel_room_participant", parent.get("attempt_id"))})
    rows = [r for r in artifacts if r.get("attempt_id") == attempt.get("attempt_id")
            and r.get("task_id") == attempt.get("task_id") and r.get("kind") in {"delegate", "call", "ask"}]
    if not rows or type(rows[-1].get("exit")) is not int:
        return False
    data = saved._indexed(repo, rows[-1], 1024 * 1024)
    _, exit_code = saved.artifact_sections_stream(__import__("io").BytesIO(data))
    return exit_code == str(rows[-1]["exit"])


def _apply(repo, attempt, target, code, task, revision, reason=None):
    command = ["bash", str(saved.ENTRY), "agent-events", "--repo", str(repo), "transition",
        "--attempt", attempt["attempt_id"], "--state", target, "--expected-state", "review",
        "--reason-code", code, "--actor", "panel-review-settlement",
        "--idempotency-key", "review-settle-" + hashlib.sha256((task + ":" + revision).encode()).hexdigest()]
    if reason:
        command.extend(["--ref", "owner_superseded_reason=" + reason])
    result = subprocess.run(command, capture_output=True, text=True, timeout=30,
                            stdin=subprocess.DEVNULL, check=False)
    if result.returncode:
        raise ValueError(clean(result.stderr, 180))


def _settle(repo, attempts, artifacts, parents, decisions, explicit=(), reason=None, apply=False):
    report = {"candidates": [], "pending": [], "errors": [], "applied": [], "executes": apply}
    # An absent plan lets retired decisions stand; a present plan that cannot be
    # read leaves generation evidence unknown, so no decision may settle.
    plan_unreadable = (os.path.lexists(os.path.join(str(repo), ".oms", "plan", "tasks.json"))
                       and not isinstance(read_plan(repo)[0].get("plan_id"), str))
    lifecycle = __import__("runpy").run_path(str(saved.ROOT / "scripts/lib/agent-events.py"))
    review_since = {}
    try:
        for row in lifecycle["read_rows"](lifecycle["event_path"](repo)):
            if row.get("event_type") == "attempt.state_changed":
                if row.get("to_state") == "review":
                    review_since[row["attempt_id"]] = row["ts"]
                elif row.get("from_state") == "review":
                    review_since.pop(row["attempt_id"], None)
    except (OSError, ValueError, KeyError, lifecycle["OpsError"]):
        pass
    by_id = {a["attempt_id"]: a for a in attempts}
    for ident in explicit:
        row = by_id.get(ident, {})
        if row.get("parent_attempt_id") not in parents or row.get("refs", {}).get("panel_role") != "worker":
            raise ValueError("selected attempt has unknown ownership or belongs to another main: " + ident)
    for attempt in attempts:
        ident, task = attempt["attempt_id"], attempt.get("task_id", "")
        if attempt.get("parent_attempt_id") not in parents or attempt.get("refs", {}).get("panel_role") != "worker":
            continue
        decision = decisions.get(task)
        chosen = ident in explicit
        if not chosen and not decision:
            continue
        if chosen:
            target, code, rev = "cancelled", "owner_superseded", hashlib.sha256(reason.encode()).hexdigest()[:24]
        else:
            payload, result_row = decision
            target, code = POLICY[payload["outcome"]]
            rev = payload["revision"]
        item = {"attempt": ident, "task": task, "state": attempt.get("state"),
                "age_in_review_seconds": _age(review_since.get(ident)),
                "selection_reason": "owner_selected" if chosen else "finalized_" + payload["outcome"],
                "target_state": target, "reason_code": code}
        if chosen:
            item["reason"] = reason
        if attempt.get("state") != "review" or target is None:
            report["pending"].append(dict(item, pending_reason="rework" if target is None else "not_in_review"))
            continue
        if not chosen:
            # As in completed_tasks: only a same-named task of the current plan
            # supersedes the decided generation; a retired plan's tasks keep it.
            if plan_unreadable:
                report["pending"].append(dict(item, pending_reason="plan_unreadable"))
                continue
            current = saved._plan_bindings(repo, {task}).get(task)
            if current is not None and payload.get("plan_binding") != current:
                report["pending"].append(dict(item, pending_reason="stale_generation"))
                continue
            # Both ledgers use second-resolution UTC timestamps; equality is
            # ambiguous, so leave it pending rather than settle newer work.
            if attempt.get("created_at", "") >= result_row.get("ts", ""):
                report["pending"].append(dict(item, pending_reason="newer_than_decision"))
                continue
        try:
            if not _stopped(repo, attempt, attempts, artifacts):
                report["pending"].append(dict(item, pending_reason="exit_unknown"))
                continue
            report["candidates"].append(item)
            if apply:
                _apply(repo, attempt, target, code, task, rev, reason if chosen else None)
                report["applied"].append(ident)
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            report["errors"].append({"attempt": ident, "error": clean(str(error), 240)})
    return report


def _decisions(repo, attempts, artifacts, parents):
    tasks = {a.get("task_id") for a in attempts if a.get("parent_attempt_id") in parents}
    latest = {r["task_id"]: r for r in artifacts if r.get("kind") == "panel-result" and r.get("task_id") in tasks}
    found = {}
    for task, row in latest.items():
        payload = saved._load_result(repo, row)
        main_id = payload.get("main_attempt_id")
        main = next((a for a in attempts if a.get("attempt_id") == main_id), {})
        if main_id in parents and payload.get("owner") == main.get("provider"):
            found[task] = (payload, row)
    return found


def after_finalize(repo, payload):
    try:
        attempts, artifacts = saved._records(repo, full=True)
        if not payload.get("main_attempt_id"):
            return {"candidates": [], "pending": [], "applied": [], "errors": [{"error": "result has no owner lineage"}], "executes": True}
        parents = _parents(repo, attempts, payload["owner"], payload["main_attempt_id"])
        row = next((r for r in artifacts if r.get("kind") == "panel-result" and r.get("task_id") == payload["task_id"]
                    and r.get("artifact", "").endswith(payload["revision"] + ".json")), None)
        if row is None:
            raise ValueError("saved result index row unavailable")
        latest = [r for r in artifacts if r.get("kind") == "panel-result" and r.get("task_id") == payload["task_id"]]
        if latest[-1] is not row:
            # A retried finalize of an older revision must not settle against a newer decision.
            return {"candidates": [], "pending": [], "applied": [], "errors": [], "executes": True,
                    "skipped": "superseded_revision"}
        return _settle(repo, attempts, artifacts, parents, {payload["task_id"]: (payload, row)}, apply=True)
    except Exception as error:
        return {"candidates": [], "pending": [], "applied": [], "errors": [{"error": clean(str(error), 240)}], "executes": True}


def settle_review(repo, owner=None, owner_attempt=None, explicit=(), reason=None, apply=False):
    saved._owner()
    if explicit and (not reason or len(reason) > 200 or any(ord(c) < 32 for c in reason)):
        raise ValueError("--attempt requires a one-line --reason of 1..200 characters")
    if reason is not None and not explicit:
        raise ValueError("--reason requires --attempt")
    def run():
        attempts, artifacts = saved._records(repo, full=True)
        parents = _parents(repo, attempts, owner, owner_attempt, require_dead=bool(owner_attempt))
        return _settle(repo, attempts, artifacts, parents, _decisions(repo, attempts, artifacts, parents),
                       explicit, reason, apply)
    if not apply:
        return run()
    # finalize publishes under this lock; selecting and settling under it keeps a
    # newer decision for the same task from racing an older one.
    with saved._lock(repo):
        return run()


def cli_main(arguments):
    from terminal_panel import repository
    parser = argparse.ArgumentParser(description="Settle stopped review workers; dry run by default.")
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--settle-review", action="store_true", required=True)
    parser.add_argument("--owner", choices=("codex", "claude"))
    parser.add_argument("--owner-attempt")
    parser.add_argument("--attempt", action="append", default=[])
    parser.add_argument("--reason")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(arguments)
    try:
        report = settle_review(repository(args.repo), args.owner, args.owner_attempt, args.attempt, args.reason, args.apply)
    except Exception as error:
        print("oms panel: " + clean(str(error), 240), file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        print("Review settlement: " + ("apply" if args.apply else "dry run"))
        for key in ("candidates", "pending", "errors"):
            for row in report[key]:
                print(key + ": " + json.dumps(row, ensure_ascii=False))
    return 1 if report["errors"] else 0
