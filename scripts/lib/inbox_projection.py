"""Pure attention projection shared by inbox and the operator cockpit."""

import calendar
import json
import os
import shlex
import subprocess
import sys
import time
import unicodedata


# Repository selection is part of each CLI's parser contract. Keep this table
# limited to commands whose --repo option is accepted by their entrypoint.
REPO_ANCHOR_MODES = {
    ("oms", "agent-events"): "repo-option",
    ("oms", "agent-plan"): "repo-option",
    ("oms", "agent-supervisor"): "repo-option",
    ("oms", "agent-task"): "repo-option",
    ("oms", "approval-inbox"): "repo-option",
    ("oms", "artifact-index"): "repo-option",
    ("oms", "change-guard"): "repo-option",
    ("oms", "fail-ledger"): "repo-option",
    ("oms", "init"): "repo-option",
    ("oms", "inbox"): "repo-option",
    ("oms", "panel"): "repo-option",
    ("oms", "patch-land"): "repo-option",
    ("oms", "plan-from-spec"): "repo-option",
    ("oms", "plan-run"): "repo-option",
    ("oms", "room"): "repo-option-and-room-env",
    ("oms", "runtime"): "repo-option",
    ("oms", "state"): "repo-option",
    ("oms", "state-verify"): "repo-option",
    ("oms", "thread"): "repo-option",
    ("git",): "git-c",
}
SHELL_OPERATORS = {";", "&&", "||", "|", "&"}


def _command_start(argv):
    """Return the executable index after leading environment assignments."""
    index = 0
    while index < len(argv) and "=" in argv[index] and not argv[index].startswith("="):
        name = argv[index].split("=", 1)[0]
        if not name or not (name[0].isalpha() or name[0] == "_") or not all(
                char.isalnum() or char == "_" for char in name):
            break
        index += 1
    return index


def has_invisible_characters(value):
    return any(unicodedata.category(char)[0] == "C" for char in str(value))


COMMAND_HIDDEN_NOTICE = (
    "(command not shown: repository path contains control or invisible characters; "
    "use oms inbox --json)"
)


def _anchor_argv(argv, repo_path):
    """Anchor a parsed command according to its verified CLI contract."""
    argv = list(argv)
    start = _command_start(argv)
    key = tuple(argv[start:start + 2]) if start < len(argv) and argv[start] == "oms" else (
        (argv[start],) if start < len(argv) and argv[start] == "git" else ())
    mode = REPO_ANCHOR_MODES.get(key)
    if mode in ("repo-option", "repo-option-and-room-env"):
        found = False
        for index in range(start + 2, len(argv)):
            if argv[index] == "--repo" and index + 1 < len(argv):
                found = True
                if not os.path.isabs(argv[index + 1]):
                    argv[index + 1] = repo_path
            elif argv[index].startswith("--repo="):
                found = True
                value = argv[index].split("=", 1)[1]
                if not os.path.isabs(value):
                    argv[index] = "--repo=" + repo_path
        if not found:
            argv[start + 2:start + 2] = ["--repo", repo_path]
        command = shlex.join(argv)
        if mode == "repo-option-and-room-env":
            for index in range(start):
                if argv[index].startswith("OMS_ROOM_REPO="):
                    argv[index] = "OMS_ROOM_REPO=" + repo_path
                    command = shlex.join(argv)
                    break
            else:
                command = "OMS_ROOM_REPO=" + shlex.quote(repo_path) + " " + command
        return command
    if mode == "git-c":
        if start + 1 < len(argv) and argv[start + 1] == "-C" and start + 2 < len(argv):
            if not os.path.isabs(argv[start + 2]):
                argv[start + 2] = repo_path
        else:
            argv[start + 1:start + 1] = ["-C", repo_path]
        return shlex.join(argv)
    return "cd " + shlex.quote(repo_path) + " && " + shlex.join(argv)


def _anchor_shell_command(command, repo_path):
    """Anchor simple pre-joined commands without changing already-qualified text."""
    lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|")
    lexer.whitespace_split = True
    lexer.commenters = ""
    tokens = list(lexer)
    if not tokens:
        return command
    if any(token in SHELL_OPERATORS for token in tokens):
        return "cd " + shlex.quote(repo_path) + " && " + command

    start = _command_start(tokens)
    key = tuple(tokens[start:start + 2]) if start < len(tokens) and tokens[start] == "oms" else (
        (tokens[start],) if start < len(tokens) and tokens[start] == "git" else ())
    mode = REPO_ANCHOR_MODES.get(key)
    if mode == "repo-option":
        has_repo = False
        needs_anchor = False
        for index in range(start + 2, len(tokens)):
            if tokens[index] == "--repo" and index + 1 < len(tokens):
                has_repo = True
                needs_anchor = needs_anchor or not os.path.isabs(tokens[index + 1])
            elif tokens[index].startswith("--repo="):
                has_repo = True
                needs_anchor = needs_anchor or not os.path.isabs(tokens[index].split("=", 1)[1])
        return command if has_repo and not needs_anchor else _anchor_argv(tokens, repo_path)
    if mode == "repo-option-and-room-env":
        has_repo = False
        needs_anchor = False
        for index in range(start + 2, len(tokens)):
            if tokens[index] == "--repo" and index + 1 < len(tokens):
                has_repo = True
                needs_anchor = needs_anchor or not os.path.isabs(tokens[index + 1])
            elif tokens[index].startswith("--repo="):
                has_repo = True
                needs_anchor = needs_anchor or not os.path.isabs(tokens[index].split("=", 1)[1])
        room_env = [token.split("=", 1)[1] for token in tokens[:start]
                    if token.startswith("OMS_ROOM_REPO=")]
        if has_repo and not needs_anchor and (not room_env or room_env == [repo_path]):
            return command if room_env else "OMS_ROOM_REPO=" + shlex.quote(repo_path) + " " + command
        return _anchor_argv(tokens, repo_path)
    if mode == "git-c":
        has_repo = start + 1 < len(tokens) and tokens[start + 1] == "-C"
        if has_repo and start + 2 < len(tokens) and os.path.isabs(tokens[start + 2]):
            return command
        return _anchor_argv(tokens, repo_path)
    return "cd " + shlex.quote(repo_path) + " && " + command


def pending_room_mail(repo):
    """Read only this caller's mailbox; retain no message bodies or receipts."""
    ident = os.environ.get("OMS_ROOM_ID")
    who = os.environ.get("OMS_ROOM_PARTICIPANT")
    result = {"bound": bool(ident or who), "healthy": True, "pending": 0,
              "oldest_age_seconds": None, "room_id": ident, "participant": who,
              "command": ""}
    if not result["bound"]:
        return result
    try:
        import room
        from room_repository import state_repository
        repository = state_repository(repo)
        ident = room.resolve(repository, room.identifier(ident, "room"))
        snapshot = room.project(room.records(repository, ident))
        pending = room.unread_messages(snapshot, who)
        now = time.time()
        ages = [max(0, int(now - calendar.timegm(time.strptime(message["ts"], "%Y-%m-%dT%H:%M:%SZ"))))
                for message in pending]
        result.update(room_id=ident, pending=len(pending), oldest_age_seconds=max(ages) if ages else None,
                      command=shlex.join(["oms", "room", "updates", "--repo", str(repository),
                                          "--id", ident, "--participant", who]))
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError, subprocess.SubprocessError):
        result.update(healthy=False, pending=None)
    return result


def project_inbox(state, *, safe_actions=(), include_threads=True, repo_path="."):
    items = []
    repo_path = os.path.realpath(repo_path)

    def repo_command(command):
        if isinstance(command, (list, tuple)):
            commands = [command] if command and isinstance(command[0], str) else command
            return "; ".join(_anchor_argv(argv, repo_path) for argv in commands)
        if not isinstance(command, str) or not command:
            return command
        try:
            return _anchor_shell_command(command, repo_path)
        except ValueError:
            return "cd " + shlex.quote(repo_path) + " && " + command

    def oms_command(*args):
        return ["oms", *args]

    def add(order, priority, code, summary, command, count=1):
        items.append({
            "order": order,
            "priority": priority,
            "code": code,
            "count": count,
            "summary": summary,
            "command": repo_command(command),
        })

    mail = state.get("room_mail", {})
    if mail.get("bound"):
        if not mail.get("healthy"):
            add(4, "P1", "room-mail-unavailable", "bound room mailbox is unavailable; check repository and participant",
                ["oms", "room", "updates"])
        elif mail.get("pending"):
            count = mail["pending"]
            add(9, "P1", "pending-room-mail", "%d unacknowledged room message(s); oldest %ss" %
                (count, mail["oldest_age_seconds"]), mail["command"], count)

    plan = state.get("plan", {})
    stale = plan.get("stale", [])
    if stale:
        add(10, "P1", "stale-plan-claim", "%d expired plan claim(s)" % len(stale),
            oms_command("agent-plan", "reclaim"), len(stale))

    operations = state.get("agent_operations", {})
    operation_states = operations.get("by_state", {})
    if not operations.get("healthy", True):
        add(5, "P1", "agent-lifecycle-corrupt", "agent lifecycle stream is invalid",
            oms_command("agent-events", "validate"))
    for state_name, code, summary, command, order, priority in (
        ("blocked", "agent-blocked", "blocked agent attempt(s)",
         oms_command("agent-events", "list", "--state", "blocked"), 11, "P1"),
        ("waiting_input", "agent-waiting-input", "agent attempt(s) waiting for input",
         oms_command("agent-events", "list", "--state", "waiting_input"), 12, "P1"),
        ("waiting_approval", "agent-waiting-approval", "agent attempt(s) waiting for approval",
         oms_command("agent-events", "list", "--state", "waiting_approval"), 13, "P1"),
        ("queued", "agent-queued", "queued agent attempt(s) not yet dispatched",
         oms_command("agent-supervisor", "dispatch"), 14, "P2"),
    ):
        count = int(operation_states.get(state_name, 0) or 0)
        if count:
            add(order, priority, code, "%d %s" % (count, summary), command, count)

    review_count = int(operations.get("review", operation_states.get("review", 0)) or 0)
    aged_review_count = min(review_count, int(operations.get("review_aged", 0) or 0))
    fresh_review_count = review_count - aged_review_count
    if fresh_review_count:
        add(15, "P2", "agent-review-fresh", "%d agent attempt(s) recently entered review" % fresh_review_count,
            oms_command("agent-events", "list", "--state", "review"), fresh_review_count)
    if aged_review_count:
        add(16, "P2", "agent-review-aged", "%d agent attempt(s) awaiting owner review; settle when decided" % aged_review_count,
            ["oms", "panel", "--settle-review"], aged_review_count)

    approvals = state.get("approvals", {})
    if not approvals.get("healthy", True):
        add(6, "P1", "approval-store-corrupt", "private approval stream is invalid",
            oms_command("approval-inbox", "validate"))
    pending_approvals = int(approvals.get("pending", 0) or 0)
    if pending_approvals:
        add(16, "P1", "pending-approval", "%d approval action(s) need a decision or completion" % pending_approvals,
            oms_command("approval-inbox", "list", "--pending"), pending_approvals)

    runtime = state.get("runtime", {})
    if not runtime.get("healthy", False):
        add(7, "P1", "runtime-core-unavailable", "typed task/evidence projection is unavailable",
            ["oms", "runtime", "doctor", "--strict"])
    else:
        evidence_counts = runtime.get("evidence", {}).get("counts", {})
        failed = int(evidence_counts.get("failed", 0) or 0)
        stale_evidence = int(evidence_counts.get("stale", 0) or 0)
        missing_evidence = int(evidence_counts.get("missing", 0) or 0)
        if failed:
            add(17, "P1", "runtime-evidence-failed", "%d acceptance criterion/criteria have failing evidence" % failed,
                ["oms", "runtime", "evidence", "show"], failed)
        if stale_evidence:
            add(75, "P2", "runtime-evidence-stale", "%d acceptance criterion/criteria have stale evidence" % stale_evidence,
                ["oms", "runtime", "evidence", "show"], stale_evidence)
        if missing_evidence:
            add(85, "P3", "runtime-evidence-missing", "%d acceptance criterion/criteria lack current evidence" % missing_evidence,
                ["oms", "runtime", "evidence", "show"], missing_evidence)
        latest_import = runtime.get("continuity", {}).get("latest_import", {})
        if latest_import.get("present"):
            capsule_id = str(latest_import.get("capsule_id") or "unknown")
            status = str(latest_import.get("status") or "unknown")
            add(90, "P3", "portable-capsule-review",
                "imported capsule %s is %s advisory continuity; no authority transferred" % (capsule_id, status),
                oms_command("state"))

    ci = state.get("ci", {})
    ci_state = ci.get("state")
    if ci_state == "current" and ci.get("conclusion") in (
        "failure", "timed_out", "cancelled", "startup_failure"
    ):
        add(20, "P1", "ci-failed", "latest CI conclusion is %s" % ci.get("conclusion"),
            ["oms", "ci-status"])
    elif ci_state == "stale":
        # Only where push state is unknowable does "does it describe HEAD?" remain
        # the open question; elsewhere the item below names what to actually do.
        add(20, "P1", "ci-stale", "recorded CI does not match the current HEAD",
            oms_command("state", "--refresh-ci"))
    elif ci_state == "unpushed":
        # Committed work no CI has ever seen. Not urgent like a red run, but it is
        # the one CI state with an action, and silence here is how commits sit for
        # a day behind a dashboard that only says "stale".
        ahead = int(ci.get("ahead") or 0)
        add(25, "P2", "unpushed-head",
            "%d commit(s) not pushed — CI has never seen this HEAD" % ahead,
            ["git", "push"], ahead)

    artifacts = state.get("artifacts", {})
    if not artifacts.get("healthy", True) or artifacts.get("invalid_rows", 0):
        add(9, "P1", "artifact-store-corrupt", "artifact index contains invalid rows or lineage",
            [oms_command("artifact-index", "validate"), oms_command("artifact-index", "salvage")])
    unresolved = int(artifacts.get("counts", {}).get("unresolved", 0) or 0)
    if unresolved:
        add(30, "P1", "unresolved-artifacts", "%d unresolved artifact outcome(s)" % unresolved,
            [oms_command("artifact-index", "unresolved"),
             oms_command("artifact-index", "resolve-recovered", "--dry-run")], unresolved)

    # A failing or stalled auto-updater is machine state, but it is exactly what
    # an agent resuming this repo needs to distrust (a weeks-dead timer once hid
    # an entire release behind "doctor: ok"). failed/overdue act now; a missing
    # trigger or never-run state is setup debt.
    install = state.get("install", {})
    au = str(install.get("auto_update", "") or "")
    au_detail = str(install.get("auto_update_detail", "") or "")
    if au in ("failed", "overdue"):
        add(45, "P1", "auto-update-" + au,
            "install auto-update is %s%s" % (au, ": " + au_detail if au_detail else ""),
            ["oms", "auto-update", "status"], 1)
    elif au in ("unwired", "no-run"):
        add(20, "P2", "auto-update-" + au,
            "install auto-update is %s%s" % (au, ": " + au_detail if au_detail else ""),
            ["oms", "auto-update", "status"], 1)

    failures = state.get("failures", {})
    open_failures = int(failures.get("open_total", 0) or 0)
    if not failures.get("healthy", True):
        add(8, "P1", "failure-ledger-corrupt", "failure ledger projection is invalid",
            oms_command("fail-ledger", "list"))
    # actionable/retiring: a hook row seen once auto-retires on its TTL and is
    # not worth P1 attention; deliberate records and recurring hook failures
    # are. An older repo-state without the split falls back to the old count.
    actionable = failures.get("actionable_total")
    retiring = int(failures.get("retiring_total", 0) or 0)
    if actionable is None:
        actionable = open_failures
        retiring = 0
    actionable = int(actionable or 0)
    if actionable:
        label = "%d actionable failure fingerprint(s)" % actionable
        if retiring:
            label += " (+%d retiring on TTL)" % retiring
        add(40, "P1", "open-failures", label, oms_command("fail-ledger", "list"), actionable)
    elif retiring:
        add(15, "P3", "retiring-failures",
            "%d one-shot hook failure(s) auto-retire on their TTL" % retiring,
            oms_command("fail-ledger", "list"), retiring)
    stale = int(failures.get("stale_total", 0) or 0)
    if stale:
        add(14, "P3", "stale-failures",
            "%d failure(s) recorded on an older commit; resolve or retry" % stale,
            oms_command("fail-ledger", "list"), stale)

    stale_reviews = plan.get("stale_review", [])
    if stale_reviews:
        add(50, "P1", "stale-plan-review", "%d abandoned review lease(s)" % len(stale_reviews),
            oms_command("agent-plan", "reclaim", "--include-review"), len(stale_reviews))

    guard = state.get("change_guard", {})
    if guard.get("active") and guard.get("stale"):
        add(60, "P1", "stale-change-guard", "change guard is active but stale",
            oms_command("change-guard", "status"))

    landings = state.get("landings", {}).get("outstanding", [])
    if landings:
        add(70, "P1", "interrupted-landing", "%d interrupted patch landing(s)" % len(landings),
            oms_command("state-verify"), len(landings))

    # Only abandoned threads are attention: a live conversation is not an inbox
    # item, and the advisory never asks for a close it could make itself.
    stale_threads = int(state.get("threads", {}).get("stale_open", 0) or 0)
    if stale_threads and include_threads:
        add(80, "P2", "stale-threads",
            "%d open cross-agent thread(s) idle past the stale TTL" % stale_threads,
            ["oms", "thread", "list", "--stale"], stale_threads)

    items.sort(key=lambda item: (item["priority"], item["order"], item["code"]))
    for item in items:
        item.pop("order", None)
    actions = list(safe_actions)
    recommended = [
        {key: (repo_command(action.get(key)) if key == "command" else action.get(key))
         for key in ("id", "priority", "authority", "reason", "command")}
        for action in state.get("runtime", {}).get("next_actions", [])[:3]
    ]
    return {
        "schema": 1,
        "actionable": len(items),
        "safe_actions": actions,
        "items": items,
        "recommended_actions": recommended,
        "room_mail": mail,
    }


def print_inbox(report, *, as_json=False):
    if as_json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    else:
        print("oms inbox: %d item(s)" % report["actionable"])
        for item in report["items"]:
            print("%s %s: %s" % (item["priority"], item["code"], item["summary"]))
            command = (COMMAND_HIDDEN_NOTICE if has_invisible_characters(item["command"])
                       else item["command"])
            print("  next: %s" % command)
        if report["recommended_actions"]:
            print("runtime recommendations:")
            for action in report["recommended_actions"]:
                command = action.get("command", "-")
                if has_invisible_characters(command):
                    command = COMMAND_HIDDEN_NOTICE
                print("  %s: %s" % (action.get("id", "action"), command))
        if report["safe_actions"]:
            print("safe actions: %s" % ", ".join(report["safe_actions"]))


if __name__ == "__main__":
    with open(sys.argv[1], encoding="utf-8") as handle:
        snapshot = json.load(handle)
    repo_path = sys.argv[2] if len(sys.argv) > 2 else os.getcwd()
    print_inbox(
        project_inbox(
            snapshot,
            safe_actions=filter(None, os.environ.get("OMS_INBOX_SAFE", "").split(",")),
            include_threads=os.environ.get("OMS_INBOX_THREADS") != "0",
            repo_path=repo_path,
        ),
        as_json=os.environ.get("OMS_INBOX_JSON") == "1",
    )
