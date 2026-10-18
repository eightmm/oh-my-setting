"""Native main lifecycle, enrollment, restart and handoff orchestration."""

from pathlib import Path
import hashlib
import json
from contextlib import nullcontext
import os
import panel_host
import re
import room
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

# Call-local facade imports preserve the public entry point and avoid import cycles.


class NativeShutdown(KeyboardInterrupt):
    def __init__(self, number):
        self.exit_code = 128 + number


def native_adapter(provider):
    """Explicit capability opt-in; never guess a vendor's interactive flags."""
    opted = re.split(r"[,\s]+", os.environ.get("OMS_PROVIDER_NATIVE_ADAPTERS", ""))
    if provider not in opted:
        return None
    marker = "oms-agent-adapter-" + provider
    folder = Path(os.environ.get("OMS_PROVIDER_ADAPTER_DIR") or
                  Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") /
                  "oh-my-setting/provider-adapters")
    candidate = folder / marker
    return str(candidate) if candidate.is_file() and os.access(candidate, os.X_OK) else shutil.which(marker)


def native_binary(provider):
    from terminal_panel import MAIN_HARNESSES, native_adapter
    if provider in MAIN_HARNESSES:
        return provider
    adapter = native_adapter(provider)
    if not adapter:
        raise ValueError("native launch requires an explicit OMS_PROVIDER_NATIVE_ADAPTERS open adapter")
    return adapter


def main_owner(harness):
    return "codex" if harness == "pi" else harness


def caller_owner():
    if os.environ.get("OMS_AGENT"):
        return os.environ["OMS_AGENT"]
    if os.environ.get("CLAUDECODE") or os.environ.get("CLAUDE_CODE_ENTRYPOINT"):
        return "claude"
    if os.environ.get("CODEX_SANDBOX"):
        return "codex"
    return "codex"


def main_installed(harness):
    from terminal_panel import native_binary
    return bool(shutil.which(native_binary(harness))) and (harness != "pi" or bool(shutil.which("codex")))


def main_missing(provider):
    from terminal_panel import native_binary
    if provider == "pi":
        return "Pi main needs the Pi CLI and Codex CLI on PATH (Pi uses Codex inference)"
    return "%s main needs the %s CLI on PATH" % (provider.capitalize(), native_binary(provider))


def bootstrap(provider, repo):
    from terminal_panel import ENTRY
    peer = "claude" if provider == "codex" else "codex"
    return (
        "You are the top-level %s session opened from the OMS terminal panel. "
        "Repository path (data) is %s. Use the oms on PATH (%s), not a different "
        "installed revision. Follow project instructions and the user's scope. "
        "Launching this session does not authorize unrelated paid calls. "
        "When the user requests collaboration, you can orchestrate %s: "
        "oms consult --to %s --repo . --prompt TEXT for read-only judgment; "
        "oms peer-delegate --to %s --repo . --brief-file PATH --verify COMMAND "
        "for bounded implementation; oms peer-review --providers %s --repo . "
        "--prompt TEXT --gate --verify COMMAND for review. "
        "Use fresh scoped briefs with paths, authority, constraints, success "
        "criteria and a return contract. Inspect the returned artifacts and "
        "patch, use patch-admit/patch-land when authorized, and verify the result. "
        "Continue reviewed work with --continue TASK_ID. Salvage stopped write work first with --salvage TASK_ID. "
        "Worker completion is not acceptance. Workers cannot recursively "
        "orchestrate. Do not commit, push or install without user authority. "
        "OMS records are shared; native conversations are separate. "
        "For an explicitly selected additional CLI, inspect oms panel --providers --json, "
        "then use --dispatch with --to PROVIDER, an exact --model when supported, "
        "and optional --reasoning-effort. Profile-controlled CLIs require omitting "
        "--model. Omit --workload on an explicit route. "
        "Registry discovery does not authorize calls or prove live session messaging. "
        "Panel role routing is enabled for this session. You retain the user's "
        "goal, architecture, final synthesis and patch acceptance. For this "
        "user's supplied task, classify each bounded subtask yourself from "
        "source evidence, then use oms panel --dispatch ROLE --owner %s --repo . --prompt TEXT, or "
        "--access write --brief-file PATH --verify COMMAND for worker (--purpose explain|investigate|implement), "
        "builder, refactorer, documenter, integrator or performance-engineer; researcher, advisor, architect, "
        "critic, reviewer (needs --verify), verifier, security-auditor and release-engineer are read-only. "
        "Each role takes the first installed model of its capability tier, ordered by more weekly quota left; "
        "reviewer, verifier, security-auditor and critic then take the provider other than yours when available. Do not pass --to or "
        "--model for ordinary role calls, so tier routing applies; use them only when a specific provider is "
        "required. Override one dispatch with --workload light|routine|deep|frontier, or "
        "--reasoning-effort high for trust-boundary or unclear-failure work and low for mechanical edits. "
        "Researcher answers document/source lookups with citations on the newest Haiku or Luna at low effort. "
        "Use security-auditor for risky changes (authentication, permissions, secrets, signals, install, "
        "deletion, destructive operations, scripts, hooks), verifier before declaring completion (original "
        "request against final result), architect for structural decisions, integrator when merging or "
        "resolving conflicts, performance-engineer when performance needs measurement, documenter at "
        "completion and release-engineer when deploying. These judges run one at a time when their trigger "
        "applies, never all concurrently by default. Advisor (which direction?), architect (how to structure?) "
        "and critic (when does it fail?) are separate judgments; call one when needed, not all by default. A role is a mandate, not an instance: --continue may carry a builder session into "
        "refactorer, while reviewer, verifier, security-auditor and critic always get a context apart from the "
        "implementer. "
        "Act as the control tower: by default delegate implementation, investigation and test repair to "
        "bounded workers (scoped brief with paths, constraints, success criteria and a --verify command), split "
        "separable files or steps into as many non-overlapping workers run in parallel as the work allows (there is no call cap), and keep scope, briefs, "
        "architecture, integration, review and patch admission in the sole main. "
        "Wait on each dispatched call separately (one notification per call); review, admit or re-dispatch it "
        "as soon as it finishes, never batch-wait for a wave. Keep configured parallelism full: start the next "
        "ready plan task or follow-up round in the same turn. Send remaining review findings to a worker via "
        "--continue, or a fresh scoped task at the rework limit; edit directly only when a brief would take longer. "
        "Work directly only when writing the brief would take longer than the edit, or when the step needs the "
        "main itself, such as integrating into a shared tree another session has frozen. "
        "Panel mains work from the shared plan (oms agent-plan): claim a ready task, delegate it (--task-id moves it "
        "to running), verify it, carry it through artifact inspection, integration and the required checks, run "
        "oms agent-plan review after acceptance, then record the owner result. "
        "A worker exit or review handoff is an intermediate step: continue the current authorized goal until it is "
        "done or a concrete scope, authority or runtime blocker remains. State that blocker and its next action. "
        "Use the shared OMS_ROOM_REPO for plan and result records even while editing in a scratch worktree. "
        "After an authorized land elsewhere, finish the shared task with --landed-commit SHA; local completion "
        "uses --finalize --outcome completed --verify COMMAND and preserves pending acceptance. "
        "Propose bounded new tasks with oms agent-plan add when source evidence exposes a gap. "
        "To know whether a land is running, use oms land status --repo PATH (active: true) - never pgrep. "
        "Before editing shared files, declare your scope with oms room scope --owns PATH, and pass "
        "--scope PATH to write workers; overlaps warn, they never lock. "
        "The sole main lands its work only when publication is authorized: oms scratch-worktree add --repo . starts "
        "detached. Create a local nontracking branch with git checkout --no-track -b oms/<task>, admit worker patches "
        "there, and rebase on the remote target. No upstream is needed. After verification, commit and run oms land "
        "from that worktree; it pushes the verified SHA to its target. A concurrent land exits 75 if the lock stays "
        "held past OMS_LOCK_TIMEOUT (default 300s); retry after the active land finishes. "
        "Nobody commits in the shared checkout. "
        "Give every subtask a short descriptive --task-id ID and --label TITLE; reuse the exact "
        "reviewed plan task ID when one exists. These calls record "
        "the main relationship, role, model, task and execution location. "
        "Use these role routes for bounded collaboration within this user's "
        "task without asking them to choose a model for each call. Do not "
        "invent unrelated work, call every advisor per turn, widen authority "
        "or reroute a policy denial. For a four-seat debate use oms panel "
        "--council --owner OWNER --task-id ID --label TITLE --prompt TEXT. "
        "Record an owner decision with oms panel --finalize --owner OWNER "
        "--task-id ID --summary-file PATH --outcome completed|accepted|failed|rejected; "
        "accepted requires --verify, --evidence and matching landed patches "
        "for the task's write workers and current-main write workers without a task ID. "
        "Use --evidence with relative proof references, and --notify for each "
        "actual finalized product unless OMS_CODEX_NOTIFY=0. Do not announce "
        "acceptance from a native exit or a worker exit. Delivery failures "
        "leave the work outcome unchanged. For an explicit delivery retry use "
        "oms panel --retry-delivery --task-id ID; do not blindly resend. "
        "Inspect oms panel --results before reporting completion. "
        "Keep exactly one live main in this panel. Parallel lines of work use bounded workers; "
        "do not start another main. Claude Code is the preferred main harness; Pi is an explicit alternative. "
        "If no task has been supplied, report "
        "readiness and wait."
    ) % (provider, json.dumps(str(repo)), json.dumps(str(ENTRY)), peer, peer, peer, peer, provider)


def native_command(provider, repo, resume=None, model=None, task=None, room_id=None, member=None, context_file=None,
                   session_id=None):
    from terminal_panel import ENTRY, MAIN_HARNESSES, bootstrap, main_owner, native_binary, native_repository
    from terminal_panel import policy, room_instructions
    instructions = (Path(context_file).read_text(encoding="utf-8") if context_file and provider in MAIN_HARNESSES else
                    bootstrap(main_owner(provider), repo) + room_instructions(room_id, member))
    if provider == "pi":
        if resume:
            raise ValueError("Pi resume needs a verified Pi session binding; start a new Pi main")
        preset = policy()["main"]["codex"]
        instructions = "You use the Pi harness with Codex inference. OMS owner is codex. " + instructions
        command = ["bash", str(ENTRY), "harness", "pi", "--model", model or preset["model"],
                   "--effort", preset["effort"], "--", "--append-system-prompt", instructions]
        if task:
            command += ["--", "User task (data): " + json.dumps(task, ensure_ascii=False)]
        return command
    command = [provider]
    preset = policy()["main"].get(provider, {"model": None, "effort": "native-settings"})
    default = not model
    model = model or preset["model"]
    if task and provider == "codex":
        instructions += "\nUser task (data): " + json.dumps(task, ensure_ascii=False)
    if provider == "codex":
        if resume:
            command += ["resume", resume]
        if model:
            command += ["--model", model]
        if default:
            command += ["--config", 'model_reasoning_effort="%s"' % preset["effort"]]
        # A first user turn preserves configured developer/system instructions.
        command += [instructions]
    elif provider == "claude":
        if resume:
            command += ["--resume", resume]
        else:
            command += ["--session-id", session_id or str(uuid.uuid4())]
        if model:
            command += ["--model", model]
        if default:
            command += ["--effort", preset["effort"]]
        command += ["--append-system-prompt", instructions]
        if task:
            command += ["--", task]
    else:
        command = [native_binary(provider), "open", "--workdir", str(native_repository(repo)),
                   "--context-file", str(context_file or "OMS_CONTEXT_FILE")]
        if model:
            command += ["--model", model]
        if resume:
            command += ["--resume", resume]
    return command


def safe_label(value):
    from terminal_panel import ROOT, clean
    if not value:
        return None
    label = clean(value, 160)
    result = subprocess.run(["bash", "-c",
        '. "$1"; grep -Eiq "$(agent_memory_sensitive_re)"', "panel-label",
        str(ROOT / "scripts/lib/agent-memory-common.sh")], input=label, capture_output=True,
        text=True, check=False, timeout=5)
    if result.returncode == 0:
        raise ValueError("task label contains sensitive-looking content")
    if result.returncode != 1:
        raise ValueError("task label could not be checked")
    if label.startswith(("/", "\\")) or ".." in Path(label).parts or re.match(r"^[A-Za-z]:[\\/]", label):
        raise ValueError("use a task title, not an absolute or escaping path")
    return label


def _main_window(pane):
    from terminal_panel import tmux_command
    if not re.fullmatch(r"%[0-9]+", pane):
        raise ValueError("window ownership is unavailable")
    result = subprocess.run(tmux_command("display-message", "-p", "-t", pane,
        "#{window_id}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t#{@oms_panel_repo}\t#{@oms_panel_room_participant}"),
        capture_output=True, text=True, check=False, timeout=5, stdin=subprocess.DEVNULL)
    fields = result.stdout.replace("\r", "").rstrip("\n").split("\t")
    if result.returncode or len(fields) != 5 or not re.fullmatch(r"@[0-9]+", fields[0]):
        raise ValueError("window ownership is unavailable")
    return fields


def _main_exit_key():
    from contextlib import ExitStack
    from panel_input import TerminalInput
    import select
    saved = {}

    def interrupt(number, frame):
        from terminal_panel import NativeShutdown
        for signum in saved:
            signal.signal(signum, signal.SIG_IGN)
        raise NativeShutdown(number)

    try:
        for name in ("SIGTERM", "SIGHUP"):
            number = getattr(signal, name, None)
            if number is not None:
                saved[number] = signal.signal(number, interrupt)
        # Register cleanup before entering: a signal may arrive during raw-mode setup.
        with ExitStack() as cleanup:
            terminal = TerminalInput()
            cleanup.callback(terminal.__exit__)
            inputs = terminal.__enter__()
            if inputs.fd is None:
                key = input()
                return "\n" if not key else key
            try:
                import termios
            except ImportError:
                termios = None
            if termios is not None:
                flush_error = getattr(termios, "error", None)
                flush_errors = (OSError, ImportError) + ((flush_error,) if isinstance(flush_error, type)
                                             and issubclass(flush_error, BaseException) else ())
                try:
                    # Native CLI typeahead must not close or restart its exit window.
                    termios.tcflush(inputs.fd, termios.TCIFLUSH)
                except flush_errors:
                    pass
            select.select([inputs.fd], [], [])
            return os.read(inputs.fd, 1).decode("utf-8", errors="replace")
    finally:
        for number, previous in saved.items():
            signal.signal(number, previous)


def _main_terminal_owner(provider, repo, session, pane, attempt, ident):
    from terminal_panel import _main_window, main_history, main_owner, session_owner
    window = _main_window(pane)
    if (session_owner(session) != str(Path(repo).resolve())
            or window[1:4] != [attempt, "", str(Path(repo).resolve())]):
        raise ValueError("restart ownership is unverified; existing window preserved")
    previous = next((row for row in main_history(repo, ident)
                     if row.get("attempt_id") == attempt), None)
    if (not previous or previous.get("terminal") is not True
            or previous.get("tool") != "panel-main"
            or previous.get("provider") != main_owner(provider)
            or previous.get("refs", {}).get("panel_role") != "main"
            or not room.same_room(repo, previous.get("refs", {}).get("panel_room_id"), ident)
            or previous.get("refs", {}).get("panel_room_participant", attempt) != window[4]):
        raise ValueError("previous main has no confirmed terminal owner; restart refused")
    resume = previous.get("refs", {}).get("native_session") if provider != "pi" else None
    if provider != "pi":
        member = room.participant(room.status(repo, ident), window[4])
        if (not member["joined"] or member["role"] != "main"
                or member["provider"] != main_owner(provider)
                or resume and member.get("consumer") and member["consumer"] !=
                    hashlib.sha256(resume.encode()).hexdigest()[:32]):
            raise ValueError("current native session binding is unverified; restart refused")
    return previous


def launch_failed_without_child(error):
    """Popen checks arguments before forking and reaps a child whose exec failed; any other launch error
    cannot prove that no native process survived."""
    return isinstance(error, (FileNotFoundError, PermissionError, NotADirectoryError, ValueError))


def run_native(provider, repo, resume=None, model=None, task=None):
    from terminal_panel import MAX_LIVE_MAINS, _lock, _main_exit_key, _main_terminal_owner, _main_window
    from terminal_panel import _native_resume_binding, check_legacy_main, clean, live_mains, managed_session
    from terminal_panel import run_single_native, session_stored, tmux_command
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("workers cannot open native owner sessions")
    if resume:
        # Reject foreign rooms before competing for the process-lifetime lease.
        # run_single_native repeats this check under the native-session lock.
        _, bound_rooms = _native_resume_binding(provider, repo, resume)
        selected = room.resolve(repo, os.environ.get("OMS_ROOM_ID"))
        if len(bound_rooms) == 1 and selected and bound_rooms - {selected}:
            raise ValueError("native session is bound to another room; resume refused")
    attempt, ident = None, None
    pane = os.environ.get("TMUX_PANE", "")
    session = os.environ.get("OMS_PANEL_SESSION", "")
    while True:
        exit_info = {}
        try:
            # Release the process-lifetime lease while the terminal exit prompt waits.
            with _lock(repo, "panel-main-session", timeout=.1):
                if attempt:
                    previous = _main_terminal_owner(provider, repo, session, pane, attempt, ident)
                    release_dead_mains(repo)
                    if live_mains(repo, session, ident) >= MAX_LIVE_MAINS:
                        raise ValueError("another main is live or unverified; finish it before restarting here")
                    resume = previous.get("refs", {}).get("native_session") if provider != "pi" else None
                    if resume and not session_stored(provider, resume):
                        resume = None
                check_legacy_main(repo)
                original_room = os.environ.get("OMS_ROOM_ID")
                try:
                    if attempt:
                        os.environ["OMS_ROOM_ID"] = ident
                    status = run_single_native(provider, repo, resume, model, None if attempt else task,
                                               _exit_info=exit_info)
                finally:
                    if original_room is None:
                        os.environ.pop("OMS_ROOM_ID", None)
                    else:
                        os.environ["OMS_ROOM_ID"] = original_room
                if not (managed_session() and sys.stdin.isatty() and sys.stdout.isatty()):
                    return status
                # Keep this invocation's identity, even if a window tag has since been replaced.
                attempt = exit_info.get("attempt")
                ident = exit_info.get("room")
                if not attempt or not ident or not re.fullmatch(r"%[0-9]+", pane):
                    print("Exit window ownership is unavailable; window preserved.", file=sys.stderr)
                    return status
                print("%s session exited (%s)." % (provider, status))
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            if not attempt:
                raise
            if exit_info.get("attempt") and exit_info.get("room"):
                try:
                    # Startup can fail after replacing the window tag. Adopt only
                    # a terminal owner so the prompt cannot act on a live successor.
                    _main_terminal_owner(provider, repo, session, pane, exit_info["attempt"], exit_info["room"])
                except (OSError, ValueError, subprocess.SubprocessError) as ownership_error:
                    print("Exit window ownership is unverified; window preserved: " + clean(str(ownership_error), 200),
                          file=sys.stderr)
                else:
                    attempt, ident = exit_info["attempt"], exit_info["room"]
                    status = 1
            message = ("another main is live or unverified; main-session lease is busy"
                       if str(error) == "panel operation is busy; retry later" else str(error))
            print("Restart refused: " + clean(message, 200), file=sys.stderr)
        while True:
            print("r resume/restart here · Enter close window", flush=True)
            try:
                key = _main_exit_key()
            except EOFError:
                return status
            if key == "r":
                break
            if key in ("\r", "\n"):
                try:
                    window = _main_window(pane)
                    if (window[1:4] != [attempt, "", str(Path(repo).resolve())]
                            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", attempt)):
                        print("Close refused: attempt, native pane or repository ownership changed; window preserved.",
                              file=sys.stderr)
                        return status
                    # Pin the window and recheck ownership on the server at deletion,
                    # so moving the pane or replacing the attempt cannot kill a successor.
                    repository = str(Path(repo).resolve())
                    # Use only literal format operands; ambiguous paths fail closed.
                    if not re.fullmatch(r"[^#,:{}\\\x00-\x1f\x7f]+", repository):
                        raise ValueError("repository path cannot be safely embedded in the close guard")
                    condition = ("#{&&:#{==:#{@oms_panel_repo},%s},"
                                 "#{&&:#{==:#{@oms_panel_main_attempt},%s},#{==:#{@oms_panel_native_pane},}}}") % (repository, attempt)
                    subprocess.run(tmux_command("if-shell", "-F", "-t", window[0], condition,
                                                "kill-window -t " + window[0]), check=True, timeout=5,
                                   stdin=subprocess.DEVNULL)
                except (OSError, ValueError, subprocess.SubprocessError) as error:
                    print("Close unavailable; window preserved: " + clean(str(error), 140), file=sys.stderr)
                return status
            if not key:
                return status


def _native_resume_binding(provider, repo, resume):
    digest = hashlib.sha256(resume.encode()).hexdigest()[:32]
    states, incomplete = room.discover(repo)
    if incomplete:
        raise ValueError("native resume ownership discovery is incomplete; inspect existing sessions")
    bound_rooms = {state["id"] for state in states for enrolled in state["participants"]
                   if enrolled["provider"] == provider and enrolled["joined"]
                   and enrolled.get("consumer") == digest}
    return states, bound_rooms


def run_single_native(provider, repo, resume=None, model=None, task=None, _exit_info=None):
    from terminal_panel import _lock, _native_resume_binding, _run_native, ensure_room, main_history
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("workers cannot open native owner sessions")
    if resume:
        key = hashlib.sha256((provider + "\0" + resume).encode()).hexdigest()[:32]
        with _lock(repo, "panel-native-" + key):
            digest = hashlib.sha256(resume.encode()).hexdigest()[:32]
            states, bound_rooms = _native_resume_binding(provider, repo, resume)
            for state in states:
                for enrolled in state["participants"]:
                    if enrolled["provider"] != provider or digest not in (enrolled.get("consumer"), enrolled.get("initial_consumer")):
                        continue
                    history = main_history(repo, state["id"], enrolled["participant"])
                    current = enrolled.get("consumer") == digest
                    claims = [row for row in history if current or not row.get("refs", {}).get("panel_session_digest")
                              or row["refs"]["panel_session_digest"] == digest]
                    if (not claims or any((row.get("provider") != provider and not (
                            row.get("terminal") is True and row.get("reason_code") == "main_swapped"))
                            or row.get("tool") != "panel-main"
                            or row.get("refs", {}).get("panel_role") != "main" or row.get("terminal") is not True
                            or not room.same_room(repo, row.get("refs", {}).get("panel_room_id", state["id"]), state["id"])
                            or row.get("refs", {}).get("panel_room_participant", row.get("attempt_id")) != enrolled["participant"]
                            for row in claims)):
                        raise ValueError("native session has no confirmed terminal owner; inspect its existing session before resuming")
            if len(bound_rooms) > 1:
                raise ValueError("native session is bound to multiple rooms; resume refused")
            ident = ensure_room(repo, os.environ.get("OMS_ROOM_ID") or next(iter(bound_rooms), None))
            if bound_rooms - {ident}:
                raise ValueError("native session is bound to another room; resume refused")
            return _run_native(provider, repo, resume, model, task, ident, _exit_info=_exit_info)
    return _run_native(provider, repo, resume, model, task, _exit_info=_exit_info)


def main_history(repo, ident, member=None):
    from terminal_panel import events
    rows = json.loads(events(repo, "list", "--json", output=True))
    if not isinstance(rows, list) or any(not isinstance(row, dict) or not isinstance(row.get("refs", {}), dict) for row in rows):
        raise ValueError("native owner evidence unavailable")
    enrolled = {p["participant"] for p in room.status(repo, ident)["participants"]}
    if member is not None:
        return [row for row in rows if row.get("attempt_id") == member or row.get("refs", {}).get("panel_room_participant") == member]
    return [row for row in rows if room.same_room(repo, row.get("refs", {}).get("panel_room_id"), ident)
            or row.get("attempt_id") in enrolled or row.get("refs", {}).get("panel_room_participant") in enrolled]


def _run_native(provider, repo, resume=None, model=None, task=None, room_id=None, _exit_info=None):
    from terminal_panel import MAIN_HARNESSES, NativeShutdown, PROVIDERS, _latest_swap, _lock, bootstrap
    from terminal_panel import child_environment, clean, ensure_room, events, launch_failed_without_child
    from terminal_panel import lineage_ids, main_history, main_installed, main_missing, main_owner
    from terminal_panel import managed_session, native_command, native_repository, pane_option, policy
    from terminal_panel import room_instructions, safe_label, tmux_command, update_activity
    harness = provider
    execution = native_repository(repo)
    if not main_installed(harness):
        raise ValueError(main_missing(harness))
    if harness == "pi" and resume:
        raise ValueError("Pi resume needs a verified Pi session binding; start a new Pi main")
    provider = main_owner(harness)
    native_session = resume or (str(uuid.uuid4()) if harness == "claude" else None)
    preset = policy()["main"].get(provider, {"model": "provider-default", "effort": "native-settings"})
    room_id = room_id or ensure_room(repo)
    member = None
    if not resume and managed_session():
        takeover = pane_option("@oms_panel_swap_participant")
        if takeover:
            state = room.status(repo, room_id)
            enrolled = room.participant(state, takeover)
            swap = _latest_swap(repo, state)
            if (enrolled["role"] != "main" or enrolled["provider"] != provider
                    or not swap or swap["participant"] != takeover or swap["to"] != provider
                    or swap.get("attempt") != pane_option("@oms_panel_swap_attempt")):
                raise ValueError("main swap lineage is unverified")
            member = takeover
    if resume:
        digest = hashlib.sha256(resume.encode("utf-8")).hexdigest()[:32]
        prior = next((p for p in room.status(repo, room_id)["participants"]
                      if p.get("consumer") == digest and p["joined"]), None)
        if prior:
            history = main_history(repo, room_id, prior["participant"])
            if (prior["provider"] != provider or prior["role"] != "main" or not history
                    or any(row.get("tool") != "panel-main" or (row.get("provider") != provider and not (
                           row.get("terminal") is True and row.get("reason_code") == "main_swapped"))
                           or row.get("refs", {}).get("panel_role") != "main" or row.get("terminal") is not True
                           or not room.same_room(repo, row.get("refs", {}).get("panel_room_id", room_id), room_id)
                           or row.get("refs", {}).get("panel_room_participant", prior["participant"]) != prior["participant"]
                           for row in history)):
                raise ValueError("native session has no confirmed terminal owner; inspect its existing session before resuming")
            member = prior["participant"]
            # A resumed main keeps the room id its lineage started in, even after that room rolled over:
            # completion proofs require every attempt of one participant to name the same room.
            started = {row.get("refs", {}).get("panel_room_id") for row in history} - {None}
            if len(started) == 1:
                room_id = next(iter(started))
    try:
        label = clean(safe_label(task), 120) or "Native task not recorded"
    except ValueError:
        # A native user task may reference private files; never copy that text
        # into shared status when it cannot serve as a safe display title.
        label = "Native task not recorded"
    import panel_release
    host_refs = panel_release.start_refs(execution, managed_session())
    status = 1
    process = None
    launch_inflight = False
    context = None
    reason = "native_exit"

    def stop_native():
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    saved_handlers = {}
    def native_interrupt(signum, frame):
        # A second hangup must not interrupt child shutdown or lifecycle writes.
        from terminal_panel import NativeShutdown
        for number in saved_handlers:
            signal.signal(number, signal.SIG_IGN)
        raise NativeShutdown(signum)

    def native_turn_interrupt(signum, frame):
        # The foreground native CLI handles its own Ctrl-C. A Python handler
        # resets on exec; SIG_IGN would instead disable the child's handler.
        pass

    attempt = events(repo, "start", "--provider", provider, "--tool", "panel-main",
                     "--ref", "panel_role=main", "--ref", "panel_owner=" + provider,
                     "--ref", "panel_harness=" + harness,
                     "--ref", "panel_model=" + (model or preset["model"]),
                     "--ref", "panel_effort=" + (preset["effort"] if not model or harness == "pi" else "native-settings"),
                     "--ref", "panel_label=" + label,
                     "--ref", "panel_location=repository", "--ref", "panel_room_id=" + room_id,
                     *(["--ref", "native_session=" + native_session,
                        "--ref", "panel_session_digest=" + hashlib.sha256(native_session.encode()).hexdigest()[:32]]
                       if native_session else []),
                     *(["--ref", "panel_room_participant=" + member] if member else []),
                     *[arg for key, value in host_refs.items() for arg in ("--ref", key + "=" + value)],
                     "--then", "starting", "--then", "working", output=True)
    try:
        native_member = member or attempt
        if _exit_info is not None:
            _exit_info.update(attempt=attempt, room=room_id)
        try:
            if not member:
                room.join(repo, room_id, native_member, provider, "main", model or preset["model"], label, native_session)
            elif not resume and native_session:
                room.bind(repo, room_id, native_member, native_session)
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            reason = "room_join_failed"
            raise
        env = child_environment(provider, repo)
        env.update(OMS_PANEL_OWNER=provider, OMS_PANEL_HARNESS=harness)
        env.update(OMS_ROOM_ID=room_id, OMS_ROOM_PARTICIPANT=native_member, OMS_ROOM_REPO=str(repo))
        if provider in PROVIDERS and harness != "pi":
            env["OMS_HOOK_AGENT"] = provider
        else:
            env.pop("OMS_HOOK_AGENT", None)
        env["OMS_PANEL_MAIN_ATTEMPT"] = attempt
        env["OMS_PANEL_RESULTS"] = "1"
        # A resumed owner is independent of any worker attempt inherited by the shell.
        env.pop("OMS_ATTEMPT_ID", None)
        for key in ("OMS_PANEL_DISPATCH", "OMS_PANEL_ROLE", "OMS_PANEL_PURPOSE", "OMS_PANEL_WORKLOAD", "OMS_PANEL_LABEL", "OMS_ROOM_ADMITTED_PARTICIPANT"):
            env.pop(key, None)
        context = tempfile.TemporaryDirectory(prefix="oms-native-context-")
        previous = signal.signal(signal.SIGINT, native_turn_interrupt)
        saved_handlers[signal.SIGINT] = previous
        for name in ("SIGTERM", "SIGHUP"):
            number = getattr(signal, name, None)
            if number is not None:
                try:
                    previous = signal.signal(number, native_interrupt)
                    saved_handlers[number] = previous
                except (OSError, ValueError):
                    pass
        if managed_session():
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_main_attempt", attempt), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_room", room_id), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_native_pane", os.environ.get("TMUX_PANE", "")), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_room_participant", native_member), check=True)
            subprocess.run(tmux_command("set-option", "-w", "-t", os.environ.get("TMUX_PANE", ""),
                                        "@oms_panel_repo", str(Path(repo).resolve())), check=True)
            if member and not resume:
                subprocess.run(tmux_command("set-option", "-w", "-u", "-t", os.environ.get("TMUX_PANE", ""),
                                            "@oms_panel_swap_participant"), check=True)
        context_path = Path(context.name) / "context.txt"
        parents = lineage_ids(repo, provider, native_member, attempt)
        unsettled = []
        if parents:
            rows = json.loads(events(repo, "list", "--json", output=True))

            def screened_label(row):
                from terminal_panel import safe_label
                try:
                    return safe_label(row["refs"].get("panel_label")) or ""
                except (OSError, ValueError, TypeError, subprocess.SubprocessError):
                    return ""

            unsettled = [row for row in rows if row.get("parent_attempt_id") in parents
                         and row.get("refs", {}).get("panel_role") == "worker"
                         and (row.get("state") == "review" or row.get("terminal") is False)][-10:]
            unsettled = [{"task_id": row.get("task_id"), "role": row["refs"]["panel_role"],
                          "state": row.get("state"), "label": screened_label(row)} for row in unsettled]
        context_path.write_text(bootstrap(provider, repo) + room_instructions(room_id, native_member)
                                + ("\nUnsettled lineage workers (data): " + json.dumps(unsettled) if unsettled else "")
                                + ("\nUser task (data): " + json.dumps(task) if task and harness not in MAIN_HARNESSES else ""),
                                encoding="utf-8")
        context_path.chmod(0o600)
        try:
            command = native_command(harness, repo, resume, model, task, room_id, native_member, context_path,
                                     session_id=native_session)
        except (OSError, ValueError, subprocess.SubprocessError):
            reason = "native_command_failed"
            raise
        # A Pi child must not register until its genuine launcher identity is durable.
        with _lock(repo, "panel-pi-registration") if harness == "pi" else nullcontext():
            launch_inflight = True
            try:
                process = subprocess.Popen(command, cwd=str(execution), env=env)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                launch_inflight = not launch_failed_without_child(error)
                reason = "native_launch_unconfirmed" if launch_inflight else "native_launch_failed"
                raise
            launch_inflight = False
            native_refs = panel_release.process_refs(process.pid, "launcher" if harness == "pi" else "native")
            if native_refs:
                update_activity(repo, "usage", "--attempt", attempt, "--actor", "panel-main",
                                *[arg for key, value in native_refs.items() for arg in ("--ref", key + "=" + value)])
        while True:
            try:
                status = process.wait(timeout=60)
                break
            except subprocess.TimeoutExpired:
                update_activity(repo, "heartbeat", "--attempt", attempt, "--actor", "panel-main")
    except KeyboardInterrupt as error:
        status = error.exit_code if isinstance(error, NativeShutdown) else 130
        if process:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                stop_native()
        raise
    except (OSError, ValueError, subprocess.SubprocessError):
        stop_native()
        raise
    finally:
        try:
            try:
                if context is not None:
                    context.cleanup()
            except OSError:
                print("Native context cleanup unavailable; lifecycle evidence is retained.", file=sys.stderr)
            native_exit = process.poll() if process is not None else None
            if not launch_inflight and type(native_exit) is int:
                exit_kind = "launcher" if harness == "pi" else "native"
                update_activity(repo, "usage", "--attempt", attempt, "--actor", "panel-main",
                                "--ref", "panel_" + exit_kind + "_exit=confirmed",
                                "--ref", "panel_" + exit_kind + "_exit_code=" + str(native_exit))
            swapped = (managed_session() and pane_option("@oms_panel_swap_attempt") == attempt
                       and not launch_inflight and (process is None or process.poll() is not None))
            if launch_inflight or process and process.poll() is None:
                update_activity(repo, "transition", "--attempt", attempt, "--state", "blocked",
                                "--actor", "panel-main", "--reason-code", "native_shutdown_unconfirmed")
            elif swapped:
                update_activity(repo, "transition", "--attempt", attempt, "--state", "blocked", "--then", "abandoned",
                                "--actor", "panel-main", "--reason-code", "main_swapped")
            elif status == 0:
                update_activity(repo, "transition", "--attempt", attempt, "--state", "verifying", "--then", "review",
                       "--then", "done", "--actor", "panel-main", "--reason-code", reason)
            else:
                update_activity(repo, "transition", "--attempt", attempt, "--state", "cancelled" if status in (129, 130, 143) else "failed",
                       "--actor", "panel-main", "--reason-code", reason)
            if managed_session() and not launch_inflight and (process is None or process.poll() is not None):
                if pane_option("@oms_panel_main_attempt") == attempt:
                    try:
                        subprocess.run(tmux_command("set-option", "-w", "-u", "-t", os.environ.get("TMUX_PANE", ""),
                                                    "@oms_panel_native_pane"), check=True, timeout=5)
                    except (OSError, subprocess.SubprocessError):
                        print("Native pane status unavailable; chat navigation may need inspection.", file=sys.stderr)
        finally:
            for number, previous in saved_handlers.items():
                signal.signal(number, previous)
    return status


def launch_command(repo, session, launch, resume=None, model=None, task=None):
    from terminal_panel import ENTRY, panel_environment, shell_command
    command = panel_environment() + ["OMS_PANEL_SESSION=" + session, "bash", str(ENTRY),
                                     "panel", "--repo", str(repo), "--host", "inline", "--launch", launch]
    for flag, value in (("--resume", resume), ("--model", model), ("--task", task)):
        if value:
            command += [flag, value]
    return shell_command(command)


def legacy_session(repo):
    """A single pre-existing random-named panel proven for this checkout is adopted, never several."""
    from terminal_panel import panel_panes, session_candidates
    try:
        found = {row["session"] for row in panel_panes(repo)}
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    found.difference_update(session_candidates(repo))
    return found.pop() if len(found) == 1 else None


def panel_mains():
    """The installed main selected by OMS_PANEL_MAIN, or the default installed main; empty opens none."""
    from terminal_panel import MAIN_HARNESSES, main_installed
    value = os.environ.get("OMS_PANEL_MAIN")
    if value is not None and value.strip() and value.strip() not in MAIN_HARNESSES:
        print("Warning: OMS_PANEL_MAIN accepts one of %s (or empty to open no main); no main opened."
              % ", ".join(MAIN_HARNESSES), file=sys.stderr)
    names = MAIN_HARNESSES if value is None else [value.strip()]
    return next(([name] for name in names if name in MAIN_HARNESSES and main_installed(name)), [])


def add_main_window(repo, session, launch, view, attention_only, env, resume=None, model=None, task=None):
    from terminal_panel import bind_window, board_split, launch_command, native_repository, tmux_command
    from terminal_panel import watch_shell
    pane = subprocess.check_output(tmux_command("new-window", "-d", "-P", "-F", "#{pane_id}", "-t", "=" + session + ":",
                                   "-n", launch, "-c", str(native_repository(repo)),
                                   launch_command(repo, session, launch, resume, model, task)),
                                   env=env, text=True).strip()
    bind_window(pane, launch, view, attention_only)
    board_split(pane, repo, watch_shell(repo, launch), main_watch=True)
    return pane


MAX_LIVE_MAINS = 1


def live_mains(repo, session, room_id, pending_window=None):
    """Panel-wide: joined mains with a live panel-main attempt in any room this panel's windows belong to, plus windows opened but not yet enrolled."""
    from terminal_panel import MAIN_HARNESSES, main_history, tmux_command
    found = subprocess.run(tmux_command("list-windows", "-t", "=" + session, "-F",
                                        "#{@oms_panel_owner}\t#{@oms_panel_room}\t#{@oms_panel_room_participant}\t#{window_name}\t#{window_id}"),
                           capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
    if found.returncode:
        raise ValueError("main window status unavailable; existing sessions preserved")
    windows = [r for r in (line.replace("\r", "").split("\t") for line in found.stdout.splitlines())
               if len(r) == 5 and (r[2] or r[3] in MAIN_HARNESSES) and r[0]
               and not (pending_window and r[4] == pending_window and not r[2])]
    live = set()
    terminal = set()
    windows = [[r[0], room.resolve(repo, r[1]) if r[1] else r[1]] + r[2:] for r in windows]
    room_id = room.resolve(repo, room_id)
    for ident in sorted({room_id} | {r[1] for r in windows if r[1]}):
        try:
            joined = {p["participant"] for p in room.status(repo, ident)["participants"]
                      if p["joined"] and p["role"] == "main"}
            history = main_history(repo, ident)
        except (ValueError, OSError):
            raise ValueError("main lifecycle status unavailable; existing sessions preserved")
        for row in history:
            refs = row.get("refs", {})
            who = refs.get("panel_room_participant", row.get("attempt_id"))
            if row.get("tool") == "panel-main":
                if row.get("terminal") is True:
                    terminal.add((ident, who))
                elif who in joined:
                    live.add((ident, who))
    # Missing lifecycle evidence cannot make an existing main available for replacement.
    live.update((r[1], r[2]) for r in windows if r[2] and (r[1], r[2]) not in terminal)
    return len(live) + sum(1 for r in windows if not r[2])


def live_main_refusal(repo, history=None):
    from terminal_panel import events
    if history is None:
        history = json.loads(events(repo, "list", "--json", output=True))
    import panel_release
    if not isinstance(history, list) or any(not isinstance(row, dict) for row in history):
        raise ValueError("main lifecycle status unavailable; existing sessions preserved")
    candidates = [row for row in history
                  if (row.get("tool") == "panel-main" or (row.get("panel") or {}).get("role") == "main")
                  and row.get("terminal") is not True and row.get("state") not in
                  {"done", "failed", "cancelled", "timed_out", "abandoned"}]
    hint = panel_release.refusal_hint(candidates)
    # A projected occupying main identifies the recovery target, not proof of termination.
    def occupies(row):
        if (row.get("panel") or {}).get("role") == "main" or "refs" not in row:
            return True
        if (row.get("refs") or {}).get("panel_role") == "main":
            try:
                return panel_release.heartbeat_fresh(row)
            except ValueError:
                pass
        return False
    occupying = [row for row in candidates if occupies(row)]
    attempt = occupying[0].get("attempt_id") if len(occupying) == 1 else None
    if isinstance(attempt, str) and panel_release.ID.fullmatch(attempt):
        # An older recoverable attempt must not displace the main occupying this panel.
        command = "oms panel --release-main " + attempt
    elif occupying:
        return "this panel already has a live main or unverified terminal owner" + panel_release.refusal_hint([])
    elif "oms panel --release-main " in hint:
        command = hint[hint.index("oms panel"):].replace(" (see k)", "")
    else:
        return "this panel already has a live main or unverified terminal owner" + hint
    summary = "this panel already has a live main or unverified owner; if crashed:"
    refusal = summary + " " + command
    # Accepted attempt IDs can exceed the summary limit; keep the command copyable.
    return refusal if len(refusal) <= 140 else summary + "\n" + command


def release_dead_mains(repo):
    """A crash or reboot leaves its main open; start abandons it on the same proof --release-main requires."""
    import panel_release
    from terminal_panel import events
    def history():
        rows = json.loads(events(repo, "list", "--json", output=True))
        if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
            raise ValueError("main lifecycle status unavailable; existing sessions preserved")
        return rows
    rows, released = history(), False
    for item in panel_release.classify(rows)["releasable"]:
        try:
            panel_release.release_held(repo, item["attempt"])
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            continue
        released = True
        print("OMS: released crashed main %s (host and native termination proven)" % item["attempt"], file=sys.stderr)
    return history() if released else rows


def release_unleased_dead_mains(repo):
    """Release before counting mains; a held lease is a live main here, which the count then refuses."""
    from terminal_panel import _lock
    entered = False
    try:
        with _lock(repo, "panel-main-session", timeout=.1):
            entered = True
            release_dead_mains(repo)
    except ValueError:
        if entered:
            raise


def check_legacy_main(repo):
    """Pre-lease mains must also finish before an inline or host launch can replace them."""
    from terminal_panel import MAX_LIVE_MAINS, legacy_session, live_main_refusal, live_mains
    from terminal_panel import managed_session, recorded_room, resolve_session, session_owner, tmux_command
    history = release_dead_mains(repo)
    if any(row.get("tool") == "panel-main" and row.get("terminal") is not True for row in history):
        raise ValueError(live_main_refusal(repo, history))
    if shutil.which("tmux"):
        session, owner = resolve_session(repo, wait=False)
        if owner != str(repo):
            legacy = legacy_session(repo)
            if legacy and session_owner(legacy) == str(repo):
                session, owner = legacy, str(repo)
        if owner == str(repo):
            ident = recorded_room(session)
            if not ident:
                raise ValueError("main room status unavailable; existing sessions preserved")
            pending = None
            if managed_session() and os.environ.get("OMS_PANEL_SESSION") == session:
                current = subprocess.run(tmux_command("display-message", "-p", "-t", os.environ.get("TMUX_PANE", ""),
                                                      "#{window_id}"), capture_output=True, text=True,
                                         check=False, timeout=3, stdin=subprocess.DEVNULL)
                value = current.stdout.replace("\r", "").strip()
                if current.returncode or not re.fullmatch(r"@[0-9]+", value):
                    raise ValueError("current main window status unavailable")
                pending = value
            if live_mains(repo, session, ident, pending) >= MAX_LIVE_MAINS:
                raise ValueError(live_main_refusal(repo, history))


def spawn_main(repo, provider, task=None, model=None):
    from terminal_panel import _lock, spawn_single_main
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("a worker cannot open owner sessions or orchestrate peers; return the need to its parent")
    with _lock(repo, "panel-main-start", timeout=3):
        return spawn_single_main(repo, provider, task, model)


def swap_choices(repo, ident):
    """Installed main harnesses with exact configured models and recorded weekly readings."""
    from terminal_panel import MAIN_HARNESSES, main_installed, main_owner, policy, routing_status
    readings = routing_status(repo, ident)
    defaults = policy()["main"]
    choices = []
    for harness in MAIN_HARNESSES:
        if not main_installed(harness):
            continue
        owner = main_owner(harness)
        model = defaults[owner]["model"]
        reading = readings.get(owner, {})
        used = reading.get("weekly_used")
        left = 100 - used if type(used) is int and 0 <= used <= 100 else None
        choices.append({"provider": harness, "model": model, "weekly_left": left})
    return choices


def _swap_main_window(repo, session):
    from terminal_panel import main_history, tmux_command
    found = subprocess.run(tmux_command("list-windows", "-t", "=" + session, "-F",
                       "#{window_id}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t"
                       "#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_panel_repo}"),
                       capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
    if found.returncode:
        raise ValueError("main window status unavailable; swap refused")
    candidates = []
    for line in found.stdout.replace("\r", "").splitlines():
        fields = line.split("\t")
        if len(fields) != 6 or not fields[1]:
            continue
        window, attempt, pane, member, ident, owner = fields
        if not re.fullmatch(r"att_[0-9a-f]{32}", attempt):
            raise ValueError("main window binding is unverified; swap refused")
        history = main_history(repo, room.resolve(repo, ident))
        active = next((row for row in history if row.get("attempt_id") == attempt), None)
        if active and active.get("terminal") is True:
            continue
        if (not re.fullmatch(r"@[0-9]+", window) or not re.fullmatch(r"%[0-9]+", pane)
                or owner != str(repo)
                or not ident or not member):
            raise ValueError("main window binding is unverified; swap refused")
        if active is None or active.get("terminal") is not False:
            raise ValueError("main lifecycle is unverified; swap refused")
        candidates.append((window, attempt, pane, member, room.resolve(repo, ident)))
    if len(candidates) != 1:
        raise ValueError("swap requires exactly one live main window")
    return candidates[0]


def _swap_digest(repo, ident, member):
    from terminal_panel import ENTRY, clean, main_history
    state = room.status(repo, ident)
    history = main_history(repo, ident)
    active = [row for row in history if row.get("refs", {}).get("panel_role") == "worker"
              and row.get("terminal") is False]
    plan = subprocess.run(["bash", str(ENTRY), "agent-plan", "--repo", str(repo), "list", "--json"],
                          capture_output=True, text=True, check=False, timeout=10, stdin=subprocess.DEVNULL)
    try:
        listed = json.loads(plan.stdout) if plan.returncode == 0 else {}
        tasks = listed.get("tasks", []) if isinstance(listed, dict) else []
    except (ValueError, TypeError):
        tasks = []
    if isinstance(tasks, dict):
        tasks = list(tasks.values())
    if not isinstance(tasks, list):
        tasks = []
    statuses = [m["text"] for m in state.get("messages", [])
                if m.get("sender") == member and m.get("message_kind") == "status"]
    decisions = [m["text"] for m in state.get("messages", []) if m.get("message_kind") == "decision"]
    lines = ["Automatic OMS handoff; verify against live state.",
             "Goal: " + clean(statuses[-1] if statuses else state.get("title", "unrecorded"), 350),
             "Decisions: " + clean("; ".join(decisions[-3:]) or "none recorded", 650),
             "In-flight workers: " + clean("; ".join("%s task=%s state=%s" %
                 (row.get("attempt_id"), row.get("task_id") or "-", row.get("state"))
                 for row in active[-12:]) or "none recorded", 900),
             "Landed commits: " + clean(", ".join(str(t.get("landed_commit")) for t in tasks
                 if isinstance(t, dict) and t.get("landed_commit")) or "none recorded", 350),
             "Next tasks: " + clean("; ".join("%s %s" % (t.get("id"), t.get("state")) for t in tasks
                 if isinstance(t, dict) and t.get("state") in {"ready", "claimed", "running", "review", "landing"})[:900]
                 or "inspect shared plan and room", 950)]
    return "\n".join(lines)


def _latest_swap(repo, state, chain=None):
    """Find a swap even if the room rolled over before its new native CLI started."""
    if state.get("swaps"):
        return state["swaps"][-1]
    for old, *_ in (room.lineage(repo, state) if chain is None else chain):
        prior = room.project(room.records(repo, old))
        if prior.get("swaps"):
            return prior["swaps"][-1]
    return None


def _swap_handoff_file(repo, ident, attempt, handoff, supplied):
    """Keep the bounded handoff beside existing session handoffs for recovery."""
    directory = Path(repo) / ".oms" / "handoffs"
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", ident):
        raise ValueError("invalid handoff room id")
    if not re.fullmatch(r"att_[0-9a-f]{32}", attempt):
        raise ValueError("invalid handoff attempt")
    if (directory.parent.is_symlink() or directory.parent.resolve() != Path(repo).resolve() / ".oms"):
        raise ValueError("room state directory is unsafe")
    directory.mkdir(mode=0o700, exist_ok=True)
    if directory.is_symlink() or directory.resolve() != (Path(repo).resolve() / ".oms" / "handoffs"):
        raise ValueError("handoff state directory is unsafe")
    path = directory / ("room-%s-swap-%s.md" % (ident, attempt))
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(str(path), flags, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            output.write("Room: %s\nFrom attempt: %s\nSource: %s\n\n%s\n" % (
                ident, attempt, "main" if supplied else "automatic_digest", handoff))
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    return path


def _swap_submit(pane, text):
    """Type into a native CLI and submit; an Enter inside the typed burst reads as a pasted newline."""
    from terminal_panel import tmux_command
    subprocess.run(tmux_command("send-keys", "-t", pane, "-l", text), check=True, timeout=3, stdin=subprocess.DEVNULL)
    time.sleep(float(os.environ.get("OMS_PANEL_SWAP_SUBMIT_DELAY", ".8")))
    subprocess.run(tmux_command("send-keys", "-t", pane, "Enter"), check=True, timeout=3, stdin=subprocess.DEVNULL)


def swap_main(repo, harness, model=None):
    """Ask for a handoff, let the native CLI exit, then reuse its proven pane and participant."""
    from terminal_panel import ENTRY, MAIN_HARNESSES, _lock, _main_window, _swap_digest, _swap_handoff_file
    from terminal_panel import _swap_main_window, _swap_submit, launch_command, live_mains, main_history
    from terminal_panel import main_installed, main_owner, native_repository, policy, provider_catalog
    from terminal_panel import resolve_session, tmux_command
    if not shutil.which("tmux") or harness not in MAIN_HARNESSES or not main_installed(harness):
        raise ValueError("swap target is unknown or not installed")
    owner = main_owner(harness)
    configured = policy()["main"][owner]["model"]
    model = model or configured
    if not re.fullmatch(r"[A-Za-z0-9_./:-]{1,160}", model):
        raise ValueError("invalid exact swap model")
    if model != configured:
        catalog = provider_catalog(owner)[0]
        if model not in catalog.get("catalog_models", catalog.get("models", [])):
            raise ValueError("swap model is not in the installed provider catalog")
    with _lock(repo, "panel-main-start", timeout=.1):
        land = subprocess.run(["bash", str(ENTRY), "land", "status", "--repo", str(repo),
                               "--active-exit", "--json"],
                              capture_output=True, text=True, check=False, timeout=10, stdin=subprocess.DEVNULL)
        status = json.loads(land.stdout) if land.returncode in (0, 3, 4) and land.stdout.strip() else None
        if not isinstance(status, dict):
            raise ValueError("land status is unavailable; swap refused")
        # flock leaves its file behind, so a finished land reads as unknown; only a running receipt blocks then.
        if land.returncode == 3 or status.get("active") is True or (
                land.returncode == 4 and status.get("state") == "running"):
            raise ValueError("land is in progress; swap refused")
        session, session_repo = resolve_session(repo, wait=False)
        if session_repo != str(repo):
            raise ValueError("no panel main to swap")
        window, attempt, pane, member, ident = _swap_main_window(repo, session)
        if live_mains(repo, session, ident) != 1:
            raise ValueError("main liveness is unverified; swap refused")
        old = next((r for r in main_history(repo, ident) if r.get("attempt_id") == attempt), None)
        if (not old or old.get("tool") != "panel-main" or old.get("terminal") is not False
                or old.get("state") != "working" or old.get("refs", {}).get("panel_room_participant", attempt) != member
                or old.get("refs", {}).get("panel_host") != "tmux"
                or old.get("refs", {}).get("panel_harness") == harness and old.get("refs", {}).get("panel_model") == model):
            raise ValueError("current main ownership or requested change is unverified")
        state = room.status(repo, ident)
        current = room.participant(state, member)
        if current["role"] != "main" or current["provider"] != old["provider"]:
            raise ValueError("current room main binding is unverified")
        nonce = "swap-" + uuid.uuid4().hex
        request = ("Please hand off the current goal before a panel main swap. At a safe point, send one "
                   "bounded room handoff (max 3000 bytes) covering goal, decisions, in-flight worker/task IDs, "
                   "landed commits and next steps. Use oms room send --id %s --participant %s --to all "
                   "--kind handoff --text 'SWAP_HANDOFF %s: <summary>'. Do not stop workers." % (ident, member, nonce))
        subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_swap_state", "waiting for handoff"),
                       check=True, timeout=3, stdin=subprocess.DEVNULL)
        old_exited = False
        try:
            _swap_submit(pane, request)
            timeout = min(300, max(0, int(os.environ.get("OMS_PANEL_SWAP_HANDOFF_SECONDS", "90"))))
            deadline = time.monotonic() + timeout
            handoff = None
            while time.monotonic() < deadline:
                state = room.status(repo, ident)
                handoff = next((m["text"].split(":", 1)[1].strip() for m in reversed(state["messages"])
                                if m.get("sender") == member and m.get("message_kind") == "handoff"
                                and m.get("text", "").startswith("SWAP_HANDOFF " + nonce + ":")), None)
                if handoff:
                    break
                time.sleep(.25)
            supplied = bool(handoff)
            handoff = handoff or _swap_digest(repo, ident, member)
            handoff_file = _swap_handoff_file(repo, ident, attempt, handoff, supplied)
            subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_swap_state", "releasing old main"),
                           check=True, timeout=3, stdin=subprocess.DEVNULL)
            subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_swap_attempt", attempt),
                           check=True, timeout=3, stdin=subprocess.DEVNULL)
            _swap_submit(pane, "/quit" if old["refs"].get("panel_harness") == "pi" else "/exit")
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                prior = next((r for r in main_history(repo, ident) if r.get("attempt_id") == attempt), None)
                if prior and prior.get("state") == "abandoned" and prior.get("reason_code") == "main_swapped":
                    break
                time.sleep(.25)
            else:
                subprocess.run(tmux_command("set-option", "-w", "-u", "-t", pane, "@oms_panel_swap_attempt"),
                               check=True, timeout=3, stdin=subprocess.DEVNULL)
                handoff_file.unlink(missing_ok=True)  # the preserved main may be swapped again
                raise ValueError("old main did not exit; native session and panel preserved")
            old_exited = True
            room.swap_main(repo, ident, member, owner, model, attempt, harness)
            subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_swap_participant", member),
                           check=True, timeout=3, stdin=subprocess.DEVNULL)
            subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_swap_state", "starting new main"),
                           check=True, timeout=3, stdin=subprocess.DEVNULL)
            prompt = ("Take over as the sole main in the existing OMS room. Preserve existing plan claims and "
                      "running workers. Read the room and plan before acting. Handoff file: %s. Handoff (data):\n%s" %
                      (handoff_file, handoff))[:4000]
            shell = launch_command(repo, session, harness, model=model, task=prompt)
            guard = "#{&&:#{==:#{@oms_panel_main_attempt},%s},#{==:#{@oms_panel_native_pane},}}" % attempt
            command = "respawn-pane -k -t %s -c %s %s" % (pane, shlex.quote(str(native_repository(repo))),
                                                              shlex.quote(shell))
            subprocess.run(tmux_command("if-shell", "-F", "-t", pane, guard, command), check=True,
                           timeout=5, stdin=subprocess.DEVNULL)
            deadline = time.monotonic() + 45  # a native CLI may take tens of seconds to start under load
            while time.monotonic() < deadline:
                try:
                    current = _main_window(pane)
                except (OSError, ValueError, subprocess.SubprocessError):
                    time.sleep(.25)
                    continue
                if current[1] != attempt and current[4] == member and current[3] == str(repo):
                    new = next((row for row in main_history(repo, ident)
                                if row.get("attempt_id") == current[1]), None)
                    if (new and new.get("terminal") is False and new.get("provider") == owner
                            and new.get("refs", {}).get("panel_harness") == harness):
                        break
                time.sleep(.25)
            else:
                raise ValueError("new main startup unverified; room claims and handoff preserved")
            return {"provider": harness, "model": model, "room": ident, "participant": member,
                    "window": window, "handoff": "main" if supplied else "automatic_digest"}
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            if old_exited and old["refs"].get("panel_harness") == "pi":
                # A fresh takeover needs no Pi resume binding; rollback still cannot resume Pi safely.
                raise ValueError("Pi main exited and will not be restored; no verified replacement main; "
                                 "room claims and handoff preserved at %s: %s" % (handoff_file, error)) from error
            raise
        finally:
            subprocess.run(tmux_command("set-option", "-w", "-u", "-t", pane, "@oms_panel_swap_state"),
                           capture_output=True, check=False, timeout=3, stdin=subprocess.DEVNULL)


def spawn_single_main(repo, provider, task=None, model=None):
    """Add a main window to this checkout's existing panel without attaching or taking focus."""
    from terminal_panel import MAX_LIVE_MAINS, _lock, add_main_window, board_view, check_legacy_main
    from terminal_panel import legacy_session, live_main_refusal, live_mains, main_installed, main_owner
    from terminal_panel import panel_room, resolve_session, session_owner, tmux_command
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("a worker cannot open owner sessions or orchestrate peers; return the need to its parent")
    if not shutil.which("tmux") or not main_installed(provider):
        raise ValueError("starting a main needs tmux and the %s CLI on PATH" % provider)
    session, owner = resolve_session(repo, wait=False)
    if owner != str(repo):
        legacy = legacy_session(repo)
        session, owner = (legacy, str(repo)) if legacy and session_owner(legacy) == str(repo) else (session, owner)
    if owner != str(repo):
        raise ValueError("no OMS panel is open for this checkout; open one with oms panel")
    env = os.environ.copy()
    env["OMS_PANEL_SESSION"] = session
    ident = os.environ["OMS_ROOM_ID"] = panel_room(repo, session)
    release_unleased_dead_mains(repo)
    if live_mains(repo, session, ident) >= MAX_LIVE_MAINS:
        raise ValueError(live_main_refusal(repo))
    with _lock(repo, "panel-main-session", timeout=.1):
        check_legacy_main(repo)
    pane = add_main_window(repo, session, provider, board_view("auto"), False, env, None, model, task)
    index = subprocess.run(tmux_command("display-message", "-p", "-t", pane, "#{window_index}"),
                           capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL).stdout.strip()
    return {"window": int(index) if index.isdigit() else None, "provider": provider,
            "owner": main_owner(provider), "room": ident}


def split_panel(repo, view="auto", attention_only=False, launch=None, resume=None, model=None, task=None, retry=True):
    from terminal_panel import _lock, bind_main_keys, construct_panel, legacy_session, resolve_session
    from terminal_panel import session_owner, split_panel, tmux_command
    chosen_room = os.environ.get("OMS_ROOM_ID")
    session, owner = resolve_session(repo, wait=retry)
    legacy = legacy_session(repo) if owner is None else None
    if legacy and session_owner(legacy) in ("", str(repo)):
        session, owner = legacy, str(repo)
        subprocess.run(tmux_command("set-option", "-t", session, "@oms_panel_repo", owner), check=True)
    if owner is not None and owner != str(repo):
        raise ValueError("tmux session %s is not this checkout's OMS panel; close it or use --layout inline" % session)
    env = os.environ.copy()
    env["OMS_PANEL_SESSION"] = session
    bind_main_keys()
    try:
        with _lock(repo, "panel-main-start", timeout=3):
            pane = construct_panel(repo, session, owner, env, view, attention_only, launch, resume, model, task)
    except (OSError, subprocess.CalledProcessError):
        if owner is None and retry:
            for unused in range(20):
                if session_owner(session):
                    break
                time.sleep(.1)
            if session_owner(session) == str(repo):
                if chosen_room:
                    os.environ["OMS_ROOM_ID"] = chosen_room
                else:
                    os.environ.pop("OMS_ROOM_ID", None)
                return split_panel(repo, view, attention_only, launch, resume, model, task, retry=False)
        raise
    attach = "switch-client" if os.environ.get("TMUX") else "attach-session"
    result = subprocess.call(tmux_command(attach, "-t", "=" + session))
    print("OMS session %s remains available while its panes are running." % session)
    return result


def construct_panel(repo, session, owner, env, view, attention_only, launch, resume, model, task):
    # Recheck after taking the construction lock: another caller may have created it while we waited.
    from terminal_panel import MAX_LIVE_MAINS, _lock, add_main_window, bind_window, board_split
    from terminal_panel import check_legacy_main, clean, ensure_room, launch_command, live_main_refusal
    from terminal_panel import live_mains, native_repository, panel_mains, panel_room, panel_shell
    from terminal_panel import session_owner, tmux_command, watch_shell
    current_owner = session_owner(session)
    if current_owner:
        if current_owner != str(repo):
            raise ValueError("tmux session is not this checkout's OMS panel")
        owner = current_owner
    created = pane = None
    try:
        if owner is not None:
            # The project's panel already exists: add the requested native window, then attach.
            if launch:
                # An explicit --room wins; otherwise new mains default to the panel's open room.
                os.environ["OMS_ROOM_ID"] = panel_room(repo, session)
                release_unleased_dead_mains(repo)
                if live_mains(repo, session, os.environ["OMS_ROOM_ID"]) >= MAX_LIVE_MAINS:
                    raise ValueError(live_main_refusal(repo))
                with _lock(repo, "panel-main-session", timeout=.1):
                    check_legacy_main(repo)
                created = "window"
                pane = add_main_window(repo, session, launch, view, attention_only, env, resume, model, task)
                subprocess.run(tmux_command("select-window", "-t", pane), check=True)
                subprocess.run(tmux_command("select-pane", "-t", pane), check=True)
        else:
            # A new panel opens one preferred main and retains a separate control window.
            mains = [] if launch else panel_mains()
            if launch or mains:
                with _lock(repo, "panel-main-session", timeout=.1):
                    check_legacy_main(repo)
            if launch or mains:
                os.environ["OMS_ROOM_ID"] = ensure_room(repo)
            if launch:
                shell = launch_command(repo, session, launch, resume, model, task)
            else:
                shell = panel_shell(repo, session, view, attention_only)
            pane = subprocess.check_output(tmux_command("new-session", "-d", "-P", "-F", "#{pane_id}",
                                          "-s", session, "-n", launch or "control", "-c", str(native_repository(repo)), shell), env=env, text=True).strip()
            created = "session"
            subprocess.run(tmux_command("set-environment", "-t", "=" + session,
                                        "OMS_PANEL_SESSION", session), check=True)
            if launch or mains:
                subprocess.run(tmux_command("set-environment", "-t", "=" + session, "OMS_ROOM_ID",
                                            os.environ["OMS_ROOM_ID"]), check=True)
            # The owner marker is written last: a racing launch joins only a fully recorded panel.
            subprocess.run(tmux_command("set-option", "-t", session, "@oms_panel_repo", str(repo)), check=True)
            if launch:
                bind_window(pane, launch, view, attention_only)
            board_split(pane, repo, watch_shell(repo, launch), main_watch=bool(launch))
            subprocess.run(tmux_command("select-pane", "-t", pane), check=True)
            opened = []
            for provider in mains:
                try:
                    opened.append(add_main_window(repo, session, provider, view, attention_only, env))
                except (OSError, subprocess.CalledProcessError) as error:
                    # The control window stays usable; a main that failed to open can be started from it.
                    print("%s main not opened: %s" % (provider, clean(str(error), 120)), file=sys.stderr)
            if opened:
                subprocess.run(tmux_command("select-window", "-t", opened[0]), check=True)
    except (OSError, subprocess.CalledProcessError):
        # Only failed construction is rolled back; an existing panel and its work are preserved.
        if created == "session":
            subprocess.run(tmux_command("kill-session", "-t", "=" + session), check=False)
        elif created == "window":
            subprocess.run(tmux_command("kill-window", "-t", pane), check=False)
        raise
    return pane


def open_native(provider, repo, resume=None, model=None, task=None, view="auto", attention_only=False):
    from terminal_panel import ENTRY, MAX_LIVE_MAINS, PANEL_SESSION, ROOT, _lock, add_main_window
    from terminal_panel import check_legacy_main, ensure_room, live_main_refusal, live_mains, main_installed
    from terminal_panel import main_owner, native_repository, panel_room, run_native, tmux_command
    execution = native_repository(repo)
    if not main_installed(provider):
        raise ValueError("%s is not installed on PATH (Pi also needs Codex CLI)" % provider)
    session = os.environ.get("OMS_PANEL_SESSION", "")
    if panel_host.herdr_active() and os.environ.get("OMS_PANEL_HOST") != "inline":
        with _lock(repo, "panel-main-session", timeout=.1):
            check_legacy_main(repo)
        os.environ["OMS_ROOM_ID"] = ensure_room(repo)
        native = ["bash", str(ROOT / "scripts" / "panel.sh"), "--repo", str(execution),
                  "--host", "herdr", "--layout", "inline", "--launch", provider]
        for flag, value in (("--resume", resume), ("--model", model), ("--task", task)):
            if value:
                native += [flag, value]
        watcher = ["bash", str(ENTRY), "panel", "--repo", str(execution), "--host", "herdr",
                   "--watch", "--owner", main_owner(provider)]
        if attention_only:
            watcher.append("--attention-only")
        panel_host.open_native(execution, provider, native, watcher)
        return 0
    if os.environ.get("TMUX") and re.fullmatch(PANEL_SESSION, session):
        with _lock(repo, "panel-main-start", timeout=3):
            ident = os.environ["OMS_ROOM_ID"] = panel_room(repo, session)
            release_unleased_dead_mains(repo)
            if live_mains(repo, session, ident) >= MAX_LIVE_MAINS:
                raise ValueError(live_main_refusal(repo))
            with _lock(repo, "panel-main-session", timeout=.1):
                check_legacy_main(repo)
            window = add_main_window(repo, session, provider, view, attention_only, os.environ.copy(),
                                     resume, model, task)
            subprocess.run(tmux_command("select-window", "-t", window), check=True)
            subprocess.run(tmux_command("select-pane", "-t", window), check=True)
        return 0
    return run_native(provider, repo, resume, model, task)
