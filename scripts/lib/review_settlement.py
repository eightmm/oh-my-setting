"""Owner-authorized lifecycle closure for stopped review workers."""
import argparse
import datetime as dt
import hashlib
import json
import math
import os
import subprocess
import sys

import panel_results as saved
from panel_routing import MAIN_OWNERS
from dashboard_projection import clean, read_plan

POLICY = {"completed": ("done", "owner_completed"), "accepted": ("done", "owner_accepted"),
          "rejected": ("cancelled", "owner_rejected"), "failed": (None, "rework")}


def _main(row):
    return (row.get("tool") == "panel-main" and row.get("refs", {}).get("panel_role") == "main"
            and row.get("provider") in MAIN_OWNERS
            and row.get("refs", {}).get("panel_owner", row.get("provider")) == row.get("provider"))


def _legacy_main(row):
    refs = row.get("refs", {})
    return not any(key in {"panel_host", "panel_pane_id", "panel_terminal_id", "panel_socket_digest"}
                   or key.startswith(("panel_wrapper_", "panel_native_", "panel_launcher_", "panel_tmux_", "panel_host_"))
                   for key in refs)


def _participant_parents(attempts, main, require_dead=False, legacy_older_than=None):
    """Validate one participant's chain; age substitutes for absent refs only by explicit recovery policy."""
    import panel_release
    from panel_view import TERMINAL_STATES
    member = main.get("refs", {}).get("panel_room_participant", main.get("attempt_id"))
    if not _main(main) or not isinstance(member, str) or not member:
        raise ValueError("review settlement ownership is unknown")
    history = [a for a in attempts if (a.get("tool") == "panel-main"
        or a.get("refs", {}).get("panel_role") == "main") and a.get("refs", {}).get(
        "panel_room_participant", a.get("attempt_id")) == member]
    ids = [row.get("attempt_id") for row in history]
    if (main not in history or any(not isinstance(ident, str) or not ident for ident in ids)
            or len(ids) != len(set(ids)) or not all(_main(row)
                and type(row.get("terminal")) is bool
                and row.get("state") in TERMINAL_STATES | {"queued", "starting", "working", "verifying",
                    "review", "blocked", "waiting_input", "waiting_approval"}
                and row["terminal"] == (row["state"] in TERMINAL_STATES) for row in history)
            or sum(row["terminal"] is False for row in history) > (0 if require_dead else 1)):
        raise ValueError("review settlement ownership is unknown")
    for row in history:
        if row["terminal"]:
            if require_dead and legacy_older_than is not None and _legacy_main(row):
                age = _age(row.get("updated_at"))
                if age is None or age <= legacy_older_than * 3600:
                    raise ValueError("legacy main last lifecycle event must be older than --legacy-older-than hours")
                continue
            panel_release.prove_host_absent(row.get("refs", {}), row["attempt_id"])
            panel_release.prove_native_absent(row)
    return set(ids)


def _parents(repo, attempts, owner=None, main_id=None, require_dead=False, legacy_older_than=None):
    import terminal_panel as panel
    if main_id:
        main = next((a for a in attempts if a.get("attempt_id") == main_id), {})
        if not _main(main) or (owner and main.get("provider") != owner):
            raise ValueError("review settlement needs a known owning main")
        if require_dead and main.get("terminal") is not True:
            raise ValueError("--owner-attempt must name a dead main")
        if require_dead:
            # The continuation helper requires one live main. Recovery instead
            # owns only this participant's fully terminal chain.
            return _participant_parents(attempts, main, require_dead=True, legacy_older_than=legacy_older_than)
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
        "--attempt", attempt["attempt_id"], "--state", target, "--expected-state", attempt.get("state", "review"),
        "--expected-seq", str(attempt["sequence"]),
        "--reason-code", code, "--actor", "panel-review-settlement",
        "--idempotency-key", "review-settle-" + hashlib.sha256((task + ":" + revision).encode()).hexdigest()]
    if reason:
        command.extend(["--ref", "owner_superseded_reason=" + reason])
    result = subprocess.run(command, capture_output=True, text=True, timeout=30,
                            stdin=subprocess.DEVNULL, check=False)
    if result.returncode:
        raise ValueError(clean(result.stderr, 180))


def _settle(repo, attempts, artifacts, parents, decisions, explicit=(), reason=None, apply=False,
            legacy_parents=None, standalone_older_than=None, aged_days=None, standalone_mode=False):
    report = {"candidates": [], "pending": [], "errors": [], "applied": [], "superseded": [], "terminal": 0, "executes": apply}
    # An absent plan lets retired decisions stand; a present plan that cannot be
    # read leaves generation evidence unknown, so no decision may settle.
    plan_unreadable = (os.path.lexists(os.path.join(str(repo), ".oms", "plan", "tasks.json"))
                       and not isinstance(read_plan(repo)[0].get("plan_id"), str))
    lifecycle = __import__("runpy").run_path(str(saved.ROOT / "scripts/lib/agent-events.py"))
    state_since = {}
    lifecycle_unreadable = False
    try:
        for row in lifecycle["read_rows"](lifecycle["event_path"](repo)):
            if row.get("event_type") == "attempt.state_changed":
                state_since[(row["attempt_id"], row.get("to_state"))] = row["ts"]
    except (OSError, ValueError, KeyError, lifecycle["OpsError"]):
        state_since = {}
        lifecycle_unreadable = True
    by_id = {a["attempt_id"]: a for a in attempts}
    for ident in explicit:
        row = by_id.get(ident, {})
        if row.get("parent_attempt_id") not in parents or row.get("refs", {}).get("panel_role") != "worker":
            raise ValueError("selected attempt has unknown ownership or belongs to another main: " + ident)
    for attempt in attempts:
        ident, task = attempt["attempt_id"], attempt.get("task_id", "")
        refs = attempt.get("refs", {})
        standalone = ((standalone_mode or standalone_older_than is not None)
                      and attempt.get("parent_attempt_id") not in by_id
                      and attempt.get("tool") != "panel-main"
                      and refs.get("panel_role") in {None, "worker"}
                      and (refs.get("panel_role") == "worker"
                           or attempt.get("tool") in {"peer-delegate", "peer-ask", "peer-call"})
                      and attempt.get("state") == "review")
        if not standalone and (attempt.get("parent_attempt_id") not in parents
                               or refs.get("panel_role") != "worker"):
            continue
        decision = decisions.get(task)
        chosen = ident in explicit
        aged = aged_days is not None
        if not chosen and not standalone and not decision and not aged:
            continue
        superseding = chosen or standalone or aged
        aged_state = attempt.get("state") if aged and attempt.get("state") in {"review", "blocked"} else "review"
        selection_reason = (("aged %s without owner decision (%g days)" % (aged_state, aged_days)) if aged
                            else "aged standalone review" if standalone else reason)
        if superseding:
            target, code, rev = "cancelled", "owner_superseded", hashlib.sha256(selection_reason.encode()).hexdigest()[:24]
        else:
            payload, result_row = decision
            target, code = POLICY[payload["outcome"]]
            rev = payload["revision"]
        age = _age(state_since.get((ident, aged_state)))
        item = {"attempt": ident, "task": task, "state": attempt.get("state"),
                "age_in_review_seconds": age,
                "selection_reason": ("aged_%s_without_owner_decision" % aged_state) if aged else "aged_standalone_review" if standalone else (
                    "owner_selected" if chosen else "finalized_" + payload["outcome"]),
                "target_state": target, "reason_code": code}
        if superseding:
            item["reason"] = selection_reason
        if legacy_parents and attempt.get("parent_attempt_id") in legacy_parents:
            item.update(selection_reason="legacy_terminal_main",
                        legacy_older_than_hours=legacy_parents[attempt["parent_attempt_id"]])
        discarded = next((row for row in payload.get("superseded_attempts", [])
                          if row.get("attempt_id") == ident), None) if not superseding else None
        if discarded and attempt.get("state") in {"failed", "timed_out", "cancelled", "abandoned"}:
            report["superseded"].append(dict(item, superseded_by=discarded["superseded_by"]))
            continue
        if attempt.get("state") in {"done", "failed", "cancelled", "timed_out", "abandoned"}:
            report["terminal"] += 1
            continue
        eligible_aged_state = aged and attempt.get("state") in {"review", "blocked"}
        if (attempt.get("state") != "review" and not eligible_aged_state) or target is None:
            pending_reason = "rework" if target is None else "aged_days_required" if chosen and attempt.get("state") == "blocked" else "not_in_review"
            report["pending"].append(dict(item, pending_reason=pending_reason))
            continue
        if aged or standalone:
            threshold = aged_days if aged else standalone_older_than
            item["aged_days" if aged else "older_than_days"] = threshold
            if age is None or age <= threshold * 86400:
                report["pending"].append(dict(item, pending_reason="review_age_unknown" if age is None else "review_too_recent"))
                continue
        if not superseding:
            # As in completed_tasks: only a same-named task of the current plan
            # supersedes the decided generation; a retired plan's tasks keep it.
            if plan_unreadable:
                report["pending"].append(dict(item, pending_reason="plan_unreadable"))
                continue
            current = saved._plan_bindings(repo, {task}).get(task)
            if current is not None and payload.get("plan_binding") != current:
                report["pending"].append(dict(item, pending_reason="stale_generation"))
                continue
            # Review entry is when the owner could have seen this attempt;
            # older records without that event retain the creation-time guard.
            # Both ledgers use second-resolution UTC timestamps, so equality
            # remains pending rather than settling ambiguous newer work.
            review_since = state_since.get((ident, "review"))
            if review_since is None and lifecycle_unreadable:
                report["pending"].append(dict(item, pending_reason="lifecycle_unreadable"))
                continue
            work_since = review_since if review_since is not None else attempt.get("created_at", "")
            if work_since >= result_row.get("ts", ""):
                report["pending"].append(dict(item, pending_reason="newer_than_decision"))
                continue
        stopped = False
        try:
            # Review is entered after execution and verification. Only the
            # explicit aged policy may replace missing exit evidence with age.
            if ((standalone and (refs.get("panel_room_participant") or refs.get("panel_room_id")))
                    or (not aged and not _stopped(repo, attempt, attempts, artifacts))):
                report["pending"].append(dict(item, pending_reason="exit_unknown"))
                continue
            stopped = True
            report["candidates"].append(item)
            if apply:
                _apply(repo, attempt, target, code, task, rev, selection_reason if superseding else None)
                report["applied"].append(ident)
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            if standalone and not stopped:
                report["pending"].append(dict(item, pending_reason="exit_unknown", error=clean(str(error), 240)))
            else:
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
        attempts, artifacts = saved._records(repo, full=True, task_id=payload["task_id"])
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


def settle_review(repo, owner=None, owner_attempt=None, explicit=(), reason=None, apply=False,
                  legacy=False, legacy_older_than=None, standalone=False, older_than=None, aged_days=None):
    saved._owner()
    if legacy and not owner_attempt:
        raise ValueError("--legacy requires --owner-attempt")
    if legacy_older_than is not None and not legacy:
        raise ValueError("--legacy-older-than requires --legacy")
    if standalone and (owner or owner_attempt or explicit or reason is not None or legacy):
        raise ValueError("--standalone cannot be combined with owner or attempt selection")
    if aged_days is not None:
        if not math.isfinite(aged_days) or aged_days < 7:
            raise ValueError("--aged-days must be finite and at least 7")
        if legacy and not owner_attempt:
            raise ValueError("--legacy requires --owner-attempt")
        if older_than is not None or explicit or reason is not None:
            raise ValueError("--aged-days cannot be combined with --older-than, --attempt or --reason")
    if older_than is not None and not standalone:
        raise ValueError("--older-than requires --standalone")
    if standalone and older_than is None and aged_days is None:
        raise ValueError("--standalone requires --older-than DAYS or --aged-days DAYS")
    for name, value in (("--legacy-older-than", legacy_older_than), ("--older-than", older_than)):
        if value is not None and (not math.isfinite(value) or value <= 0):
            raise ValueError(name + " must be finite and positive")
    legacy_hours = (24 if legacy_older_than is None else legacy_older_than) if legacy else None
    if explicit and (not reason or len(reason) > 200 or any(ord(c) < 32 for c in reason)):
        raise ValueError("--attempt requires a one-line --reason of 1..200 characters")
    if reason is not None and not explicit:
        raise ValueError("--reason requires --attempt")
    def run():
        attempts, artifacts = saved._records(repo, full=True)
        parents = set() if standalone else _parents(repo, attempts, owner, owner_attempt,
            require_dead=bool(owner_attempt), legacy_older_than=legacy_hours)
        legacy_parents = {a["attempt_id"]: legacy_hours for a in attempts
                          if legacy and a["attempt_id"] in parents and _legacy_main(a)}
        decisions = {} if aged_days is not None else _decisions(repo, attempts, artifacts, parents)
        return _settle(repo, attempts, artifacts, parents, decisions,
                       explicit, reason, apply, legacy_parents=legacy_parents,
                       standalone_older_than=older_than if standalone else None,
                       aged_days=aged_days, standalone_mode=standalone)
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
    parser.add_argument("--owner", choices=MAIN_OWNERS)
    parser.add_argument("--owner-attempt")
    parser.add_argument("--legacy", action="store_true")
    parser.add_argument("--legacy-older-than", type=float, metavar="HOURS")
    parser.add_argument("--standalone", action="store_true")
    parser.add_argument("--older-than", type=float, metavar="DAYS")
    parser.add_argument("--aged-days", type=float, metavar="DAYS")
    parser.add_argument("--attempt", action="append", default=[])
    parser.add_argument("--reason")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(arguments)
    try:
        report = settle_review(repository(args.repo), args.owner, args.owner_attempt, args.attempt, args.reason, args.apply,
                               args.legacy, args.legacy_older_than, args.standalone, args.older_than, args.aged_days)
    except Exception as error:
        print("oms panel: " + clean(str(error), 240), file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        print("Review settlement: " + ("apply" if args.apply else "dry run"))
        print("terminal: " + str(report["terminal"]))
        for key in ("candidates", "pending", "errors"):
            for row in report[key]:
                print(key + ": " + json.dumps(row, ensure_ascii=False))
    return 1 if report["errors"] else 0
