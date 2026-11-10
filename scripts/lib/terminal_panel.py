#!/usr/bin/env python3
"""Terminal front end; existing OMS tools retain state and mutation authority."""

import argparse, datetime, fnmatch, hashlib, io, json, math, os, queue, re, shlex, shutil, signal, stat, subprocess, sys, tempfile, textwrap, threading, time, uuid
from builtins import compile
from contextlib import redirect_stdout
from pathlib import Path
import room, panel_host, panel_debate, panel_messages, panel_reader, review_settlement
import dispatch_supervisor as dispatch_store
from dashboard_projection import clean
from panel_view import INBOX_LIMIT, main_occupied, menu_rows, render, render_results
from panel_input import PANEL_SESSION
from panel_routing import (allocate, alternate_route, command as routed_command, policy, KINDS, MODEL_IDS,
                           TIERS, WRITERS, MAIN_OWNERS, routable_route_model, route_family, tier_catalog)
from panel_results import (DURABLE, _read, _write, _lock, accept, finalize, outcomes, results, results_snapshot,
                           retry_delivery, verified_patch, pending_admissions, write_summary_draft)
from panel_cache import read_shared
from room_repository import EXECUTION_ENV, native_repository, state_repository, work_repository

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / "scripts" / "oms"
PROVIDERS = ("codex", "claude")
MAIN_HARNESSES = ("claude", "pi", "codex")
# Fresh Agy launch has no verified swap/history binding; retain the existing shortcut targets.
_LAUNCH_HARNESSES = (*MAIN_HARNESSES, "agy")
MAX_PROMPT = 65536
SESSION_ID = r"[A-Za-z0-9][A-Za-z0-9_-]{0,159}"
OUTBOUND_SCANNER = ROOT / "scripts" / "lib" / "peer-common.sh"

from panel_dispatch import (
    PlanBinding, provider_catalog, route_inputs, routing_status, selected_route, continuation, moved_main,
    scope_overlaps, dispatch, _validate_dispatch_verifier, plan_link, _read_dispatch_brief,
    continuation_defaults, _validate_dry_run_inputs, _rollover_if_needed, _adopt_successor, _dispatch_capacity,
    _dispatch_group_gone, _dispatch_exited, _dispatch_leader_gone, _dispatch_process_snapshot, _dispatch_session_gone,
    _dispatch_descendants, _observe_dispatch_descendants, _dispatch_descendants_gone, _dispatch_recorded_gone,
    _dispatch_call_path, _dispatch_pending_path, _dispatch_exit_record, _dispatch_output_log, _tee_dispatch_output,
    _save_dispatch_output, _dispatch_verifier, _prepare_dispatch_call, _dispatch_call_record,
    _record_dispatch_pending, _dispatch_pending, _dispatch_supervisor_snapshot, _dispatch_supervisor_receipt,
    _cancel_watch_authority, cancel_selected_call, _dispatch_call_finished, _dispatch_released, _reconcile_dispatch,
    _observe_dispatch_session, _stop_dispatch_groups, _stop_dispatch, _settle_dispatch,
    _rollback_dispatch_enrollment, run_dispatch, _print_dispatch_next_ready, council_command, council,
    peer_command, run, dashboard)
from panel_native import (
    NativeShutdown, native_adapter, native_binary, main_owner, resolve_main, caller_owner, main_installed,
    main_missing, bootstrap, native_command, safe_label, _main_window, _main_exit_key, _main_terminal_owner,
    launch_failed_without_child, run_native, _native_resume_binding, run_single_native, main_history, _run_native,
    launch_command, legacy_session, panel_mains, add_main_window, MAX_LIVE_MAINS, live_mains, live_main_refusal,
    check_legacy_main, spawn_main, swap_choices, _swap_main_window, _swap_digest, _latest_swap, _swap_handoff_file,
    _swap_submit, swap_main, spawn_single_main, split_panel, construct_panel, open_native)
from panel_layout import (
    ReloadWatcher, shell_command, panel_shell, watch_shell, panel_environment, board_view, reader_shell,
    BOARD_SHARE, BOARD_HEIGHT_SHARE, MIN_CHAT, parse_split, split_text, top_text, split_cells, NATIVE_MIN,
    board_split_cells, work_split_cells, cycle_panel_mode, terminal_window, board_split, panel_geometry,
    PanelModeSync, SplitSync, source_stamps, REJECTED_RELOAD, reload_ready, panel_panes, reopen_panel, bind_window,
    keys_help, bind_main_keys, PANEL_KEYS)
from panel_state import (
    ensure_room, room_instructions, repository, git_toplevel, panel_repository, child_environment, events,
    update_activity, active_mains, session_stored, lineage_ids, lineage_ids_from_rows, owning_attempt, tmux_command,
    session_candidates, resolve_session, session_owner, room_open, room_headroom, recorded_room, session_room,
    panel_room, MAIN_OUTPUT, MAIN_INTERRUPT_CACHE, main_activity, snapshot, refresh_failed,
    hold_state, awaiting_admission, read_panel, private_capacity, main_needs_attention,
    main_has_open_questions)
from panel_review import (
    _safe_patch_path, _AUTO_REVIEW_FRONTIER_PATHS, _automatic_review_workload, _call_result_event,
    _review_verdict, _review_dispatch_context, _rework_history, _write_brief_artifact,
    _review_fix_notice, _dispatch_execution_repository, _completion_inputs, _review_verifier, _settlement_claim_path,
    SETTLEMENTS_HELD, _settlement_step, _settlement_release, _settlement_delivery_pending, _review_launch_state,
    deliver_result, finalize_selection)
from panel_navigation import (
    _cancel_window_id, _cancel_client_name, _clear_cancel_option, _begin_cancel_confirmation, _cancel_option_value,
    _poll_cancel_confirmation, _request_cancel_confirmation, session_option, pane_option, pane_local_option,
    reset_group_view, navigate, read_evidence, finish_opening, expire_notice, window_zoomed, call_results,
    resolve_call_alias, render_call_view, ask_advisor, worker_worktree, open_worktree, tab_event,
    BOTTOM_OPTION, HEADER_OPTION, HEADER_ENABLED, context_header_text,
    _tmux_output_value, _window_option_local, update_context_header, toggle_context_headers, bottom_state,
    bottom_text, read_bottom, publish_bottom, refreshed_navigation)
from panel_render import (
    show_providers, clear_control_view, room_listing, terminal_style, managed_session, stale_notice,
    provider_availability, frame_view, _show_state, show, draw_frame)
from panel_watch import BackgroundRead, mark_window, start_spawn, reap_spawns, stop_attention_command, watch
from panel_interaction import (
    read_field, browse_results, browse_chats, raw_keys, menu_input, read_choice, interactive)

KEYS_HELP = keys_help()
# Text inside tmux's conditional cannot contain commas or closing braces.
_MAIN_COMMANDS = dict((("F9", "select-pane -t :.+"),
             ("F5", 'if-shell -F "#{==:#{window_name},control}" "select-window -l" "select-window -t :=control"'),
             ("F8", 'if-shell -F "#{==:#{@oms_panel_mode},overview}" "set-option -w @oms_panel_mode focus" '
                    '"if-shell -F \\"#{==:#{@oms_panel_mode},focus}\\" \\"set-option -w @oms_panel_mode work\\" '
                    '\\"set-option -w @oms_panel_mode overview\\""'),
             ("F12", 'display-message -d 8000 "#{?#{==:#{window_name},control},%s,%s}"' % KEYS_HELP)))
MAIN_KEYS = tuple((key, _MAIN_COMMANDS[key]) for key in dict.fromkeys(
    row[0] for row in PANEL_KEYS if "root" in row[2]))


def main(argv=None):
    arguments = list(sys.argv[1:] if argv is None else argv)
    if arguments and arguments[0] in ("terminal", "job"):
        backend_name = "panel_terminals" if arguments[0] == "terminal" else "panel_jobs"
        try:
            backend = __import__(backend_name)
        except ImportError as error:
            print("error: %s backend is unavailable: %s" % (arguments[0], error), file=sys.stderr)
            return 2
        return backend.cli_main(arguments[1:])

    parser = argparse.ArgumentParser(description="OMS terminal control panel with registered CLI peers.")
    parser.add_argument("--repo")
    parser.add_argument("--layout", choices=("auto", "inline", "split"), default="auto")
    parser.add_argument("--host", choices=("auto", "inline", "tmux", "herdr"), default="auto",
                        help="optional terminal host; Herdr requires an existing local POSIX pane")
    parser.add_argument("--room", help="existing shared work room; never joins a different task implicitly")
    parser.add_argument("--launch", help="open the sole main (claude, codex, agy or explicit pi)")
    parser.add_argument("--spawn-main", metavar="PROVIDER",
                        help="start the sole main in an empty panel without attaching; works without a terminal")
    parser.add_argument("--swap-main", metavar="PROVIDER",
                        help="replace the sole main in its panel pane after a bounded handoff")
    parser.add_argument("--reopen", action="store_true", help="restore an existing tmux board without restarting its input CLI")
    parser.add_argument("--resume", help="resume an exact native session ID (requires --launch)")
    parser.add_argument("--model", help="exact model for --launch, --spawn-main, --swap-main or --dispatch --to")
    parser.add_argument("--to", help="explicit registered CLI target for --dispatch")
    parser.add_argument("--reasoning-effort", choices=("low", "medium", "high"), help="effort for this dispatch; a role route keeps its model")
    parser.add_argument("--providers", action="store_true", help="read registered CLI capabilities without running providers")
    parser.add_argument("--release-main", metavar="ATTEMPT", help="release a crashed main after proving its terminal owner absent")
    parser.add_argument("--health", action="store_true", help="read context, reported quota, room bounds and private slot/history capacity")
    parser.add_argument("--sessions", action="store_true",
                        help="read-only normalized status of every recorded session (one line each; --json for schema 1)")
    parser.add_argument("--usage", action="store_true",
                        help="read-only usage of Claude, Codex and Antigravity as each reported it locally (--json for schema 1)")
    parser.add_argument("--metrics", action="store_true",
                        help="read-only recorded metrics; follow-up task IDs have no producer linkage")
    parser.add_argument("--since", metavar="DURATION", help="limit --metrics to records within a duration (for example 7d)")
    parser.add_argument("--min-samples", type=int, metavar="N", help="with --metrics, hide role/provider/model/effort route groups with fewer than N decided tasks (default 3)")
    parser.add_argument("--json", action="store_true", help="read-only availability and shared OMS state")
    parser.add_argument("--watch", action="store_true", help="read-only activity tree, refreshed every five seconds")
    parser.add_argument("--owner", help="main identity: claude, codex, pi or antigravity; registered adapters also support watch/dispatch/council")
    parser.add_argument("--count", type=int, help="stop after N watch snapshots")
    parser.add_argument("--no-animation", action="store_true", help="keep --watch motion-free")
    parser.add_argument("--view", choices=("auto", "summary", "compact", "detail", "graph", "tree"), help="activity density for the menu or --watch")
    parser.add_argument("--attention-only", action="store_true", help="show only calls awaiting action in the menu or --watch")
    parser.add_argument("--routes", action="store_true", help="show the panel role policy without calling models")
    parser.add_argument("--recommend", metavar="KIND",
                        help="read-only tier/effort advice for a task kind (explore, mechanical-edit, implement, debug, design, review, summarize); "
                             "needs --diff RANGE or --risk with --size; --failed N adds an escalation suggestion; --json supported")
    parser.add_argument("--risk", choices=("low", "medium", "high"), help="--recommend: risk class when no --diff is given")
    parser.add_argument("--size", choices=("small", "medium", "large"), help="--recommend: size bucket when no --diff is given")
    parser.add_argument("--diff", metavar="RANGE", help="--recommend: git diff range (for example main..HEAD) to measure risk and size")
    parser.add_argument("--failed", type=int, metavar="N", help="--recommend: failed attempts so far; 2 or more prints an escalation suggestion")
    parser.add_argument("--task", help="task for the native main (requires --launch)")
    parser.add_argument("--dispatch", choices=tuple(KINDS), metavar="ROLE", help="run one bounded role task: " + ", ".join(KINDS))
    parser.add_argument("--main", help="assert the dispatch caller's own main participant (never select another main)")
    parser.add_argument("--scope", action="append", metavar="PATH",
                        help="repeatable repo-relative write scope (default: the plan task's allowed paths, else the whole repository); conflicting writes or undecided results refuse dispatch")
    parser.add_argument("--council", action="store_true", help="run the Codex, Claude and Antigravity debate (deep tier by default)")
    parser.add_argument("--rounds", type=int, help="council debate rounds, 1 or 2; --lenses requires 0 (default with lenses)")
    parser.add_argument("--lenses", help="--council only: 2..12 unique comma-separated names [a-z0-9-]+, 1..32 characters each; one blind seat each")
    parser.add_argument("--until-agreement", action="store_true",
                        help="--council only: debate until every answering seat shares one verdict line or the cap is reached "
                             "(a tally of identical lines, not correctness; cost scales with rounds); replaces --rounds, excludes --lenses")
    parser.add_argument("--max-rounds", type=int, metavar="N", help="--until-agreement only: round cap, 1 to 6 (default 4)")
    parser.add_argument("--thread", help="council thread ID; a fresh thread prefix with --lenses")
    parser.add_argument("--results", action="store_true", help="read bounded durable task results")
    parser.add_argument("--export", metavar="FILE",
                        help="--results only: write what the reader shows to a new local file instead of the terminal")
    parser.add_argument("--page", type=int, metavar="N", help="--results only: page N (1-based, 20 tasks each) of the retained list")
    parser.add_argument("--salvage", metavar="TASK_ID", help="capture a stopped worker's kept worktree for owner review")
    parser.add_argument("--mail", metavar="PARTICIPANT", help="read messages addressed to one main in --room")
    parser.add_argument("--participant", help="--results of one room call (with --task-id, one council seat)")
    parser.add_argument("--chats", action="store_true", help="read enrolled native chats and navigation capabilities")
    parser.add_argument("--open-chat", help="navigate to a selected room participant without starting a model turn")
    parser.add_argument("--chat-surface", choices=("auto", "terminal", "app"), default="auto")
    parser.add_argument("--accept", metavar="TASK", help="commit one reviewed worker candidate locally; publication stays explicit")
    parser.add_argument("--allow-verifier-change", action="store_true", help="allow --accept to change its verifier")
    parser.add_argument("--allow-floor-change", metavar="REASON", help="owner reason for an intentional failed admission floor")
    parser.add_argument("--message", help="commit message for --accept")
    parser.add_argument("--expected-patch-sha256", help="assert the reviewed candidate digest for --accept")
    parser.add_argument("--landed-commit", help="finish the plan with a published SHA before --finalize")
    parser.add_argument("--finalize", action="store_true", help="record an owner result")
    parser.add_argument("--settle-review", action="store_true", help="list stopped review workers; --apply closes eligible attempts")
    parser.add_argument("--apply", action="store_true", help="apply --settle-review (default: dry run)")
    parser.add_argument("--owner-attempt", help="--settle-review for a named dead main's lineage")
    parser.add_argument("--legacy", action="store_true", help="allow aged terminal mains with no host/native ownership refs")
    parser.add_argument("--legacy-older-than", type=float, metavar="HOURS", help="legacy main lifecycle age (default: 24 hours)")
    parser.add_argument("--standalone", action="store_true", help="settle aged review workers with absent parents")
    parser.add_argument("--older-than", type=float, metavar="DAYS", help="minimum review age with exit proof for --standalone")
    parser.add_argument("--aged-days", type=float, metavar="DAYS", help="explicitly cancel aged review or blocked workers in the selected lineage (at least 7 days)")
    parser.add_argument("--attempt", action="append", help="--salvage: select one stopped attempt; --settle-review: supersede this worker")
    parser.add_argument("--reason", help="record why explicitly selected workers are superseded")
    parser.add_argument("--summary-file", help="bounded owner summary for --finalize")
    parser.add_argument("--outcome", choices=("completed", "accepted", "failed", "rejected"))
    parser.add_argument("--evidence", action="append", help="safe repository-relative evidence for --finalize (up to eight)")
    parser.add_argument("--verify-timeout", type=int,
                        help="finalize verifier timeout in seconds, 1..86400 (default: 1800)")
    parser.add_argument("--notify", action="store_true", help="deliver the recorded result to Codex app")
    parser.add_argument("--retry-delivery", action="store_true", help="retry delivery of a saved task result without its original summary file")
    parser.add_argument("--workload", choices=TIERS,
                        help="capability tier instead of the role's default; council accepts routine, deep or frontier")
    parser.add_argument("--access", choices=("read", "write"))
    parser.add_argument("--purpose", choices=("explain", "investigate", "implement"), help="plain worker task kind")
    parser.add_argument("--prompt", help="bounded read task for --dispatch")
    parser.add_argument("--brief-file", help="scoped task brief for --dispatch")
    parser.add_argument("--continue", dest="continue_task", metavar="TASK_ID",
                        help="continue this main's earlier worker for TASK_ID with --brief-file (worker dispatch)")
    parser.add_argument("--allow-stale-base", action="store_true",
                        help="continue on the recorded patch base even when it differs from current HEAD")
    parser.add_argument("--verify", help="mechanical verifier for --finalize or the dispatched role (required for write and reviewer; other read roles refuse it)")
    parser.add_argument("--task-id", help="existing plan/task identifier for the worker")
    parser.add_argument("--from-task", action="append", metavar="ID",
                        help="append settled read-only findings from another task (repeatable; needs --brief-file)")
    parser.add_argument("--label", help="short task title for the role board")
    parser.add_argument("--alias", help="short unique room call name (1-8 letters, digits or hyphens)")
    parser.add_argument("--dry-run", action="store_true", help="show the launch plan without starting anything")
    args = parser.parse_args(arguments)
    workload_explicit = args.workload is not None
    if args.continue_task and args.brief_file and not (args.prompt or args.task_id) and args.access in (None, "write"):
        # A follow-up round repeats the original write dispatch's inputs unless a flag overrides them.
        inherited, unavailable = None, "no recorded write dispatch for the task"
        try:
            continue_owner = args.owner or caller_owner()
            continue_repo = panel_repository(os.environ.get("OMS_ROOM_REPO") or args.repo)
            if args.room:
                room.identifier(args.room, "room")
            continue_env = child_environment(continue_owner, continue_repo)
            if args.room:
                continue_env["OMS_ROOM_ID"] = args.room
            inherited = continuation_defaults(continue_repo, continue_owner, args.main, args.continue_task,
                                              continue_env, continue_env.get("OMS_ROOM_ID"))
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            unavailable = clean(str(error), 160)
        if inherited:
            args.dispatch = args.dispatch or inherited["role"]
            args.access = "write"
            if args.dispatch == "worker" and not args.purpose:
                # Report-only (investigate) work must not turn into implement by omission.
                # Named writers record their role as purpose; a move to a plain worker keeps the implement default.
                if inherited["role"] == "worker":
                    if inherited["purpose"] not in ("investigate", "implement"):
                        parser.error("the original worker purpose was not recoverable; pass --purpose")
                    args.purpose = inherited["purpose"]
            # An explicit route (--to/--model) is incompatible with a capability tier.
            if not (args.to or args.model):
                args.workload = args.workload or inherited["workload"]
            args.scope = list(dict.fromkeys((inherited["scope"] or []) + (args.scope or []))) or None
            args.verify = args.verify or inherited["verify"]
        elif not args.dispatch:
            parser.error("--continue requires a write-capable --dispatch role with --brief-file, and takes no "
                         "--prompt or --task-id; the original role was not recoverable (%s), so pass --dispatch ROLE "
                         "--access write" % unavailable)
    args.access = args.access or "read"
    if args.settle_review:
        if args.accept or args.finalize or args.results or args.retry_delivery:
            parser.error("--settle-review cannot be combined with result actions")
        return review_settlement.cli_main(arguments)
    if args.salvage is not None:
        import panel_salvage
        return panel_salvage.cli_main(arguments)
    if (args.apply or args.owner_attempt or args.attempt or args.reason is not None or args.legacy
            or args.legacy_older_than is not None or args.standalone or args.older_than is not None or args.aged_days is not None):
        parser.error("settlement options require --settle-review; --attempt also supports --salvage")
    if args.repo is not None:
        try:
            git_toplevel(args.repo)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            # A missing directory, an unreadable path or a git timeout is as permanent as a non-repository.
            reason = str(error) if " is not a git repository" in str(error) else \
                "%s is not a usable git repository (%s); pass --repo PATH" % (args.repo, type(error).__name__)
            print("oms panel: " + clean(reason, 200), file=sys.stderr)
            return 3
    if args.release_main is not None:
        import panel_release
        return panel_release.cli_main(arguments)
    if args.health and (args.dry_run or args.view or args.attention_only):
        parser.error("--health is a read-only report; use --repo, --room and optional --json")
    if args.sessions and (args.dry_run or args.view or args.attention_only):
        parser.error("--sessions is a read-only report; use --repo, --room and optional --json")
    if args.usage and (args.dry_run or args.view or args.attention_only):
        parser.error("--usage is a read-only report; use --repo, --room and optional --json")
    if args.since is not None and not args.metrics:
        parser.error("--since requires --metrics")
    if args.min_samples is not None and (not args.metrics or args.min_samples < 1):
        parser.error("--min-samples requires --metrics and a positive number")
    if args.page is not None and not (args.results and not args.task_id):
        parser.error("--page requires --results without --task-id")
    if args.page is not None and not 1 <= args.page <= 1000:
        parser.error("--page must be between 1 and 1000")
    if args.alias is not None and not args.dispatch:
        parser.error("--alias requires --dispatch")
    if sum(bool(value) for value in (args.watch, args.launch, args.spawn_main, args.swap_main, args.reopen, args.dispatch, args.routes,
                                     args.council, args.results, args.accept, args.finalize, args.retry_delivery, args.providers,
                                     args.chats, args.open_chat, args.health, args.mail, args.metrics, args.sessions, args.recommend,
                                     args.usage)) > 1:
        parser.error("choose one panel operation")
    if args.recommend:
        import route_rules
        return route_rules.cli(args, parser.error)
    if args.risk or args.size or args.diff or args.failed is not None:
        parser.error("--risk, --size, --diff and --failed require --recommend")
    if args.json and (args.watch or args.launch or args.dispatch or args.routes or args.council or args.accept or args.finalize):
        parser.error("--json is only available for panel state or results")
    if args.open_chat and args.json and not args.dry_run:
        parser.error("chat navigation JSON requires --dry-run")
    if args.chat_surface != "auto" and not args.open_chat:
        parser.error("--chat-surface requires --open-chat")
    if args.participant and not args.results:
        parser.error("--participant requires --results")
    if args.mail and (not args.room or args.json or args.dry_run):
        parser.error("--mail requires --room and plain read-only output")
    if args.task and not (args.launch or args.spawn_main):
        parser.error("--task requires --launch or --spawn-main")
    if args.task and (len(args.task.encode("utf-8")) > MAX_PROMPT or "\0" in args.task):
        parser.error("task exceeds the panel input contract")
    if args.watch and (args.launch or args.json or args.dry_run):
        parser.error("--watch is a read-only view; cannot combine with launch, JSON or dry-run")
    if args.continue_task and (args.dispatch not in WRITERS or not args.brief_file or args.prompt or args.task_id):
        parser.error("--continue requires a write-capable --dispatch role with --brief-file, and takes no --prompt or --task-id")
    # A main names itself on every panel call; reading results accepts that identity without filtering by it.
    if args.owner is not None and not (args.watch or args.dispatch or args.council or args.accept or args.finalize or args.results):
        parser.error("--owner requires --watch, --dispatch, --council, --accept, --finalize or --results")
    if args.count is not None and not args.watch:
        parser.error("--count requires --watch")
    if args.no_animation and not args.watch:
        parser.error("--no-animation requires --watch")
    if args.spawn_main and args.dry_run:
        parser.error("--spawn-main starts a main; it has no dry-run")
    if args.swap_main and (args.dry_run or args.host not in {"auto", "tmux"} or args.layout == "inline"):
        parser.error("--swap-main requires an existing tmux panel and has no dry-run")
    if (args.view or args.attention_only) and (args.launch or args.spawn_main or args.swap_main or args.reopen or args.json or args.dry_run or args.routes
            or args.dispatch or args.council or args.results or args.accept or args.finalize or args.retry_delivery or args.providers
            or args.chats or args.open_chat or args.mail or args.metrics):
        parser.error("--view and --attention-only apply to the interactive menu or --watch")
    if (args.brief_file or args.purpose or args.access != "read" or
            (args.workload and not (args.dispatch or args.council))) and not args.dispatch:
        parser.error("role task options require --dispatch")
    if args.prompt and not (args.dispatch or args.council):
        parser.error("--prompt requires --dispatch or --council")
    if args.verify and not (args.dispatch or args.accept or args.finalize):
        parser.error("--verify requires --dispatch, --accept or --finalize")
    if args.task_id and not (args.dispatch or args.council or args.results or args.finalize or args.retry_delivery):
        parser.error("--task-id requires a task operation")
    if args.export and (not args.results or args.json):
        parser.error("--export requires --results and takes no --json")
    if args.main and not args.dispatch:
        parser.error("--main requires --dispatch")
    if args.allow_stale_base and not args.continue_task:
        parser.error("--allow-stale-base requires --continue")
    if args.scope and (args.dispatch not in WRITERS or args.access != "write"):
        parser.error("--scope requires a write-capable --dispatch role with --access write")
    if args.main:
        try:
            room.identifier(args.main, "main participant")
        except ValueError as error:
            parser.error(str(error))
    if args.label and not (args.dispatch or args.council):
        parser.error("--label requires --dispatch or --council")
    if (args.rounds is not None or args.thread or args.lenses is not None) and not args.council:
        parser.error("--rounds, --thread and --lenses require --council")
    if (args.until_agreement or args.max_rounds is not None) and not args.council:
        parser.error("--until-agreement and --max-rounds require --council")
    if args.max_rounds is not None and not args.until_agreement:
        parser.error("--max-rounds requires --until-agreement")
    if args.until_agreement and args.lenses is not None:
        parser.error("--until-agreement and --lenses are mutually exclusive: blind independent seats never see each other's answers")
    if args.until_agreement and args.rounds is not None:
        parser.error("--until-agreement replaces --rounds; set the cap with --max-rounds")
    if args.until_agreement and args.max_rounds is not None and not 1 <= args.max_rounds <= 6:
        parser.error("--max-rounds must be 1 to 6")
    if args.rounds is None and not args.until_agreement:
        args.rounds = 0 if args.lenses is not None else 1
    if (args.summary_file or args.outcome or args.evidence or args.notify or args.verify_timeout is not None) and not args.finalize:
        parser.error("finalization options require --finalize")
    if args.retry_delivery and not args.task_id:
        parser.error("--retry-delivery requires --task-id")
    if args.dry_run and (args.results or args.accept or args.finalize or args.retry_delivery or args.providers or args.chats):
        parser.error("--dry-run only plans a launch, role dispatch or council")
    if (args.message is not None or args.expected_patch_sha256 is not None) and not args.accept:
        parser.error("--message and --expected-patch-sha256 require --accept")
    if not args.accept and (args.allow_verifier_change or args.allow_floor_change is not None):
        parser.error("consent options require --accept")
    if args.allow_floor_change is not None and (not args.allow_floor_change.strip() or
            any(ord(c) < 32 or ord(c) == 127 for c in args.allow_floor_change)):
        parser.error("--allow-floor-change requires a nonblank single-line reason without control characters")
    if args.accept is not None and (not args.accept or not (args.repo and args.message and args.verify) or args.task_id):
        parser.error("--accept TASK requires --repo, --message and --verify; takes no --task-id")
    if args.landed_commit is not None and not args.finalize:
        parser.error("--landed-commit requires --finalize")
    if args.finalize and not (args.task_id and args.summary_file and args.outcome and args.owner):
        parser.error("--finalize requires --owner, --task-id, --summary-file and --outcome")
    if args.landed_commit is not None and args.outcome not in ("accepted", "completed"):
        parser.error("--landed-commit requires --outcome accepted or completed")
    if args.council:
        if not args.prompt:
            parser.error("--council requires --prompt")
        derived = []
        if args.task_id is None:
            args.task_id = "council-" + hashlib.sha256(args.prompt.encode("utf-8")).hexdigest()[:8]
            derived.append("task-id=" + args.task_id)
        if args.label is None:
            first_line = args.prompt.split("\n", 1)[0].strip()
            args.label = first_line[:60] + ("…" if len(first_line) > 60 else "")
            derived.append("label=" + json.dumps(args.label, ensure_ascii=True))
        if derived:
            print("council defaults: " + ", ".join(derived), file=sys.stderr)
    if (args.to or args.reasoning_effort) and not args.dispatch:
        parser.error("--to and --reasoning-effort require --dispatch")
    if args.council and args.workload and args.workload not in ("routine", "deep", "frontier"):
        parser.error("council --workload must be routine, deep or frontier")
    if args.task_id and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", args.task_id):
        parser.error("invalid task ID")
    if args.from_task and (not args.brief_file or any(
            not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}", value) for value in args.from_task)):
        parser.error("--from-task requires --brief-file and valid task IDs")
    if args.from_task and not args.dispatch:
        parser.error("--from-task requires --dispatch")
    if args.dispatch:
        try:
            owner = args.owner or caller_owner()
            route = selected_route(owner, args.dispatch, args.workload, args.access, args.purpose,
                                   args.to, args.model, args.reasoning_effort,
                                   repo=args.repo, room_id=args.room, main=args.main)
            # A prompt-less advisor call asks its single question interactively before routing.
            pending = args.dispatch == "advisor" and not (args.prompt or args.brief_file or args.dry_run)
            if args.brief_file:
                brief_path = Path(args.brief_file).expanduser()
                if not brief_path.is_absolute():
                    brief_path = Path(args.repo if args.repo is not None else os.getcwd()).expanduser() / brief_path
                _read_dispatch_brief(brief_path)
            routed_command(ENTRY, args.repo if args.repo is not None else os.getcwd(), route, "pending question" if pending else args.prompt,
                           args.brief_file, args.verify, args.task_id)
        except (ValueError, OSError) as error:
            parser.error(clean(str(error), 160))
    if args.count is not None and not 1 <= args.count <= 1000000:
        parser.error("--count must be between 1 and 1000000")
    if args.resume and not args.launch:
        parser.error("--resume requires --launch")
    if args.model and not (args.launch or args.dispatch or args.spawn_main or args.swap_main):
        parser.error("--model requires --launch, --spawn-main, --swap-main or --dispatch --to")
    if args.json and args.launch:
        parser.error("--json is read-only; use --dry-run for a launch plan")
    if args.reopen and (args.json or args.dry_run or args.host in {"inline", "herdr"}):
        parser.error("--reopen restores a tmux board; omit JSON, dry-run and non-tmux hosts")
    if args.json and args.dry_run and not args.open_chat:
        parser.error("choose --json or --dry-run")
    if args.resume and not re.fullmatch(SESSION_ID, args.resume):
        parser.error("invalid exact session ID")
    if args.model and (args.model.startswith("-") or len(args.model) > 160
                       or not re.fullmatch(r"[A-Za-z0-9_./:-]+", args.model)):
        parser.error("invalid model ID")
    if (args.dispatch or args.council or args.finalize or args.retry_delivery or args.chats or args.open_chat
            or args.spawn_main or args.swap_main or not (
            args.json or args.dry_run or args.watch or args.routes or args.results or args.providers or args.health or args.mail or args.metrics or args.sessions or args.usage)) and (
            os.environ.get("OMS_HARNESS_CHILD") == "1"
            or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"):
        parser.error("a worker cannot open owner sessions or orchestrate peers; return the need to its parent")
    host_control = False
    try:
        if args.providers:
            rows = provider_catalog()
            if args.json:
                print(json.dumps({"schema": 1, "kind": "oms-panel-providers", "providers": rows,
                                  "executes": False}, ensure_ascii=False, indent=2))
            else:
                show_providers(rows)
            return 0
        if args.routes:
            routes = policy()
            rows = provider_catalog(",".join(sorted({c["provider"] for tier in routes["tiers"].values() for c in tier})))
            installed = {row["provider"] for row in rows if row["installed"]}
            owner = caller_owner()
            readings, recent, _ = route_inputs(owner, args.repo, args.room)
            catalogs = {row["provider"]: tier_catalog(row) for row in rows}
            chosen = {}
            for role in routes["roles"]:
                try:
                    route = allocate(owner, role, installed=installed, provider_status=readings, recent=recent,
                                     catalogs=catalogs)
                    route["model"] = routable_route_model(route["provider"], route["model"], catalogs[route["provider"]])
                    chosen[role] = {key: route[key] for key in ("provider", "model", "effort")}
                except ValueError as error:
                    chosen[role] = {"error": str(error)}
            print(json.dumps(dict(routes, writers=sorted(WRITERS), kinds=KINDS,
                                  owner=owner, chosen=chosen), indent=2))
            return 0
        repo = panel_repository(os.environ.get("OMS_ROOM_REPO") or args.repo) if args.dispatch else panel_repository(args.repo)
        requested_worktree = work_repository(args.repo if args.repo is not None else os.getcwd())
        if not args.dispatch and requested_worktree != repo:
            os.environ[EXECUTION_ENV] = str(requested_worktree)
        execution = requested_worktree if args.dispatch else native_repository(repo)
        if args.host != "auto":
            os.environ["OMS_PANEL_HOST"] = args.host
        if args.host == "herdr" and not (args.json or args.dry_run):
            panel_host.current(execution)
        if args.host == "tmux" and args.layout == "inline":
            raise ValueError("--host tmux conflicts with --layout inline")
        if args.room:
            room.identifier(args.room, "room")
            if room.status(repo, args.room)["closed"] and not (args.chats or args.open_chat or args.health or args.sessions or args.usage):
                raise ValueError("selected room is closed")
            os.environ["OMS_ROOM_ID"] = args.room
        if args.sessions:
            import session_status
            state, status = snapshot(repo, room_id=args.room)
            read = session_status.project(state)
            print(json.dumps(read, ensure_ascii=False, indent=2) if args.json else session_status.text(read))
            return status
        if args.health:
            import panel_health
            from panel_metrics import collect
            if args.room:
                state, status = snapshot(repo, room_id=args.room)
                state["provider_status"] = collect(state, repo=repo)
                state["private_capacity"] = private_capacity(repo)
            else:
                state, status, *_ = read_panel(repo, caller_owner(), None, False)
            if args.json:
                print(json.dumps(panel_health.project(state), ensure_ascii=False))
            else:
                print(panel_health.render(state, 120, 100))
            return status
        if args.usage:
            import provider_usage
            from panel_metrics import collect
            if args.room:
                state, status = snapshot(repo, room_id=args.room)
            else:
                state, status, *_ = read_panel(repo, caller_owner(), None, False)
            # Collected even without a room participant, so the Codex session log and agy artifacts still show.
            state = dict(state, provider_status=collect(state, repo=repo))
            usage = provider_usage.snapshot(state)
            print(json.dumps(usage, ensure_ascii=False, indent=2) if args.json else provider_usage.text(usage))
            return status
        if args.metrics:
            from panel_metrics import aggregate, metrics_text
            report = aggregate(repo, args.since, min_samples=args.min_samples or 3)
            print(json.dumps(report, ensure_ascii=False, indent=2) if args.json else metrics_text(report))
            return 0
        if args.reopen:
            print(json.dumps(reopen_panel(repo)))
            return 0
        if args.chats or args.open_chat:
            import panel_chats
            ident = args.room or os.environ.get("OMS_ROOM_ID")
            if not ident:
                raise ValueError("select an existing room first")
            report = (panel_chats.catalog(repo, ident) if args.chats else
                      panel_chats.open_chat(repo, ident, args.open_chat, args.chat_surface, args.dry_run))
            print(panel_chats.text(report) if args.chats and not args.json else json.dumps(report, ensure_ascii=False, indent=2))
            return 0
        if args.launch and args.launch not in _LAUNCH_HARNESSES:
            args.launch = provider_catalog(args.launch)[0]["provider"]
            native_binary(args.launch)
        if args.launch == "pi" and args.resume:
            raise ValueError("Pi resume needs a verified Pi session binding; start a new Pi main")
        if args.launch == "agy" and args.resume:
            raise ValueError("Agy resume needs a verified native session binding; start a fresh Agy main")
        if args.spawn_main:
            if args.spawn_main not in _LAUNCH_HARNESSES:
                args.spawn_main = provider_catalog(args.spawn_main)[0]["provider"]
            report = spawn_main(repo, args.spawn_main, args.task, args.model)
            if args.json:
                print(json.dumps(report))
            else:
                print("Started %s main in window %s of room %s" % (report["provider"].capitalize(), report["window"], report["room"]))
            return 0
        if args.swap_main:
            report = swap_main(repo, args.swap_main, args.model)
            if args.json:
                print(json.dumps(report))
            else:
                print("Swapped main to %s %s in room %s" % (report["provider"], report["model"], report["room"]))
            return 0
        if args.council:
            if args.dry_run:
                output = io.StringIO()
                with redirect_stdout(output):
                    result = council(repo, args.owner or caller_owner(), args.task_id, args.label, args.prompt,
                                     args.thread, args.rounds, True, args.workload or "deep", lenses=args.lenses,
                                     until_agreement=args.until_agreement, max_rounds=args.max_rounds)
                if output.getvalue():
                    plan = json.loads(output.getvalue())
                    plan.update(task_id=args.task_id, label=args.label)
                    print(json.dumps(plan, ensure_ascii=False))
                return result
            return council(repo, args.owner or caller_owner(), args.task_id, args.label, args.prompt,
                           args.thread, args.rounds, False, args.workload or "deep", lenses=args.lenses,
                           until_agreement=args.until_agreement, max_rounds=args.max_rounds)
        if args.mail:
            participant = room.identifier(args.mail, "participant")

            def reader(width):
                state = room.status(repo, args.room)
                if not any(m["participant"] == participant and m["role"] == "main" and m["joined"]
                           for m in state["participants"]):
                    raise ValueError("main left or its role changed; refresh before reading messages")
                state = dict(state, messages=panel_messages.read(repo, args.room))
                return panel_messages.mail_page(state, participant, width)

            board = os.environ.get(panel_reader.BOARD, "")
            if panel_reader.PANE.fullmatch(board) and sys.stdin.isatty() and sys.stdout.isatty():
                return panel_reader.page(reader, board)
            print(reader(shutil.get_terminal_size().columns))
            return 0
        if args.results:
            participant = args.participant and room.identifier(args.participant, "participant")
            if args.task_id:
                args.task_id = resolve_call_alias(repo, room.resolve(repo, args.room), args.task_id)

            def reader(width, view="Summary"):
                if not participant:
                    return render_results(report(), width)
                return render_call_view(repo, participant, view, width, args.task_id, reader_cache)

            reader_cache = {}

            def report():
                read = (results(repo, args.task_id, participant) if args.page is None else
                        results(repo, args.task_id, participant, offset=(args.page - 1) * 20))
                if participant:
                    read["rows"] = [row for row in read["rows"] if row.get("calls")]
                return read

            board = os.environ.get(panel_reader.BOARD, "")
            if args.export:
                import panel_export
                return panel_export.write(args.export, reader(panel_export.WIDTH))
            if args.json:
                print(json.dumps(report(), ensure_ascii=False, indent=2))
            elif panel_reader.PANE.fullmatch(board) and sys.stdin.isatty() and sys.stdout.isatty():
                return panel_reader.page(reader, board, on_refresh=reader_cache.clear)
            else:
                print(reader(shutil.get_terminal_size().columns))
            return 0
        if args.accept:
            report = accept(repo, args.owner or caller_owner(), args.accept, args.message, args.verify,
                            args.expected_patch_sha256, execution_repo=execution,
                            allow_verifier_change=args.allow_verifier_change,
                            allow_floor_change=args.allow_floor_change)
            print("committed locally; publication pending: " + report['sha'])
            print("Next: oms land --repo " + shlex.quote(str(execution)))
            print("Then: oms panel --finalize --repo " + shlex.quote(str(execution)) + " --landed-commit <full head SHA that oms land pushed>")
            return 0
        if args.finalize:
            report = finalize(repo, args.owner, args.task_id, args.summary_file, args.outcome,
                              args.verify, args.evidence, args.notify,
                              1800 if args.verify_timeout is None else args.verify_timeout,
                              execution_repo=execution, landed_commit=args.landed_commit)
            print(json.dumps(report, ensure_ascii=False, indent=2))
            return 0
        if args.retry_delivery:
            print(json.dumps(retry_delivery(repo, args.task_id), ensure_ascii=False, indent=2))
            return 0
        if args.dispatch:
            brief = Path(args.brief_file).expanduser() if args.brief_file else None
            if brief:
                if not brief.is_absolute():
                    brief = execution / brief
                _read_dispatch_brief(brief)
                brief = brief.resolve(strict=True)
            if args.prompt and (len(args.prompt.encode("utf-8")) > MAX_PROMPT or "\0" in args.prompt):
                raise ValueError("prompt exceeds the panel input contract")
            owner = args.owner or caller_owner()
            asked = args.dispatch == "advisor" and not (args.prompt or brief or args.dry_run)
            if asked:
                if not sys.stdin.isatty():
                    raise ValueError("advisor dispatch needs --prompt or an interactive terminal")
                print("Advisor for main %s (%s). Enter sends one read-only question; Ctrl-C cancels."
                      % (args.main or "of this provider", owner))
                args.prompt = read_field("Question: ")
                if len(args.prompt.encode("utf-8")) > MAX_PROMPT or "\0" in args.prompt:
                    raise ValueError("prompt exceeds the panel input contract")
            status = dispatch(repo, owner, args.dispatch, args.workload, args.access, args.purpose,
                              args.prompt, brief, args.verify, args.task_id, args.dry_run, args.label,
                              args.to, args.model, args.reasoning_effort, main=args.main,
                              continue_task=args.continue_task, scopes=args.scope, alias=args.alias,
                              work_repo=None if args.continue_task and args.repo is None else execution,
                              allow_stale_base=args.allow_stale_base, from_task=args.from_task,
                              workload_explicit=workload_explicit)
            if asked:
                read_field("Exit %s; the answer is in the main's room mail. Press Enter to close: " % status,
                           required=False)
            return status
        if args.watch:
            provider = args.owner or caller_owner()
            if provider not in MAIN_OWNERS:
                provider = provider_catalog(provider)[0]["provider"]
            return watch(repo, provider, args.count or 0, args.no_animation,
                         args.view, args.attention_only)
        if args.json:
            state, status = snapshot(repo)
            print(json.dumps({"schema": 1, "kind": "oms-panel", "dashboard": state,
                              "providers": {name: {"installed": bool(shutil.which(name))}
                                            for name in PROVIDERS},
                              "main_harnesses": {name: {"installed": main_installed(name), "owner": main_owner(name)}
                                                 for name in _LAUNCH_HARNESSES},
                              "max_live_mains": MAX_LIVE_MAINS,
                              "split_available": bool(shutil.which("tmux")),
                              "routing": policy(),
                              "coverage": "OMS records; native sessions are separate"}))
            return status
        # The checkout's fixed tmux panel is the default home, also from inside another tmux
        # session (switch-client, no nesting) and for --launch; inline stays an explicit choice.
        split = args.host not in ("inline", "herdr") and not panel_host.herdr_active() and (
            args.host == "tmux" or args.layout == "split" or (
                args.layout == "auto" and shutil.which("tmux")))
        if args.dry_run:
            resolution = resolve_main(args.launch, args.model) if args.launch else None
            launchable = resolution and not (
                args.launch == "pi" and resolution["transport"] == "direct")
            print(json.dumps({"schema": 1, "kind": "oms-panel-launch",
                              "repo": str(repo), "layout": "split" if split else "inline",
                              "host": "tmux" if split else "herdr" if args.host == "herdr" or panel_host.herdr_active() else "inline",
                              "provider": resolution["owner"] if resolution else None,
                              "harness": args.launch,
                              "max_live_mains": MAX_LIVE_MAINS,
                              "argv": native_command(args.launch, repo, args.resume, args.model, args.task)
                              if launchable else None,
                              "room": os.environ.get("OMS_ROOM_ID") or "new on launch",
                              "environment_keys": ["OMS_AGENT", "OMS_PANEL_REPO", "OMS_PANEL_ENTRYPOINT", "PATH",
                                                   "OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "OMS_ROOM_REPO"],
                              "executes": False}))
            return 0
        if not sys.stdin.isatty() or not sys.stdout.isatty():
            parser.error("interactive panel needs a terminal; use --json or --dry-run")
        if not args.launch and args.host == "auto" and args.layout == "auto" and managed_session():
            print(json.dumps(reopen_panel(repo)))
            return 0
        # panel_shell supplies inline layout and a view, but no --host, including on reload.
        if (not args.launch and args.layout == "inline" and args.view is not None
                and (args.host == "auto" and managed_session() or panel_host.herdr_active())):
            return interactive(repo, args.view, args.attention_only)
        if split and args.launch:
            if not main_installed(args.launch):
                raise ValueError(main_missing(args.launch))
            if not shutil.which("tmux"):
                raise ValueError("native chat board requires tmux on PATH")
            return split_panel(repo, args.view or "auto", args.attention_only, args.launch, args.resume, args.model, args.task)
        if args.launch:
            if not main_installed(args.launch):
                raise ValueError(main_missing(args.launch))
            status = run_native(args.launch, repo, args.resume, args.model, args.task)
            return status
        if split:
            if not shutil.which("tmux"):
                raise ValueError("split layout needs an installed tmux; inline needs no extra dependency")
            return split_panel(repo, args.view or "auto", args.attention_only)
        from panel_native import _startup_main
        mains, resume = _startup_main(repo)
        if mains:
            sibling = panel_host.herdr_active() and os.environ.get("OMS_PANEL_HOST") != "inline"
            if not sibling:
                return run_native(mains[0], repo, resume=resume)
            open_native(mains[0], repo, resume=resume, view=args.view or "auto",
                        attention_only=args.attention_only)
        host_control = panel_host.herdr_active()
        return interactive(repo, args.view or "auto", args.attention_only)
    except ReloadWatcher:
        # The watcher restored the terminal; re-execute it on the updated, compiling sources.
        command = [sys.executable] + sys.argv
        if host_control:
            # Retain Herdr's controller lifetime after the original startup argv is re-executed.
            command += ["--layout", "inline", "--view", args.view or "auto"]
        stop_attention_command()
        os.execv(sys.executable, command)
    except NativeShutdown as error:
        return error.exit_code
    except KeyboardInterrupt:
        return 130
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        if " is not a git repository; open it in a git repository or pass --repo PATH" in str(error):
            print("oms panel: " + clean(str(error), 200), file=sys.stderr)
            return 3
        if isinstance(error, (subprocess.TimeoutExpired, subprocess.CalledProcessError)):
            command = error.cmd
            argv = command if isinstance(command, (list, tuple)) else [command]
            verb = None
            for index, argument in enumerate(argv[:-1]):
                if os.path.basename(str(argument)) == "oms":
                    verb = str(argv[index + 1])
                    break
            if not verb:
                verb = os.path.basename(str(argv[0])) if argv else "command"
            if isinstance(error, subprocess.TimeoutExpired):
                reason = "timed out after %g s" % error.timeout
            else:
                reason = "exited %s" % error.returncode
            stderr = getattr(error, "stderr", None)
            if stderr:
                if isinstance(stderr, bytes):
                    stderr = stderr.decode("utf-8", "replace")
                last_line = str(stderr).splitlines()[-1:]
                if last_line:
                    reason += ": " + clean(last_line[0], 200)
            message = "error: %s %s" % (clean(verb, 80), reason)
            message = clean(message, 207)
        else:
            message = clean("error: " + clean(str(error), 1000), 1000)
        print(message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
