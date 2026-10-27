#!/usr/bin/env python3
"""Terminal front end; existing OMS tools retain state and mutation authority."""

import argparse
from builtins import compile
from contextlib import nullcontext, redirect_stdout
import datetime
import fnmatch
import hashlib
import io
import json
import math
import os
import queue
from pathlib import Path
import re
import shlex
import signal
import shutil
import stat
import subprocess
import sys
import time
import threading
import tempfile
import textwrap
from typing import NamedTuple
import room
import uuid
import panel_host
import panel_debate
import panel_messages
import panel_reader
import dispatch_supervisor as dispatch_store
import review_settlement

from dashboard_projection import clean
from panel_view import INBOX_LIMIT, main_occupied, menu_rows, render, render_results
from panel_input import PANEL_SESSION
from panel_routing import (allocate, alternate_route, command as routed_command, policy, KINDS, MODEL_IDS,
                           TIERS, WRITERS, newest_route_model, route_family)
from panel_results import (DURABLE, _read, _write, _lock, accept, finalize, outcomes, results, results_snapshot,
                           retry_delivery, verified_patch, pending_admissions, write_summary_draft)
from panel_cache import read_shared
from room_repository import EXECUTION_ENV, native_repository, state_repository, work_repository

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / "scripts" / "oms"
PROVIDERS = ("codex", "claude")
MAIN_HARNESSES = ("claude", "pi", "codex")
MAX_PROMPT = 65536
SESSION_ID = r"[A-Za-z0-9][A-Za-z0-9_-]{0,159}"
OUTBOUND_SCANNER = ROOT / "scripts" / "lib" / "peer-common.sh"


from panel_dispatch import PlanBinding
from panel_native import NativeShutdown
from panel_layout import ReloadWatcher
from panel_native import native_adapter
from panel_native import native_binary
from panel_native import main_owner
from panel_native import caller_owner
from panel_native import main_installed
from panel_native import main_missing
from panel_state import ensure_room
from panel_state import room_instructions
from panel_dispatch import provider_catalog
from panel_dispatch import route_inputs
from panel_dispatch import routing_status
from panel_dispatch import selected_route
from panel_render import show_providers
from panel_render import clear_control_view
from panel_render import room_listing
from panel_state import repository
from panel_state import git_toplevel
from panel_state import panel_repository
from panel_state import child_environment
from panel_native import bootstrap
from panel_native import native_command
from panel_state import events
from panel_state import update_activity
from panel_native import safe_label
from panel_native import _main_window
from panel_native import _main_exit_key
from panel_native import _main_terminal_owner
from panel_native import launch_failed_without_child
from panel_native import run_native
from panel_native import _native_resume_binding
from panel_native import run_single_native
from panel_native import main_history
from panel_native import _run_native
from panel_state import active_mains
from panel_state import session_stored
from panel_state import lineage_ids
from panel_state import lineage_ids_from_rows
from panel_state import owning_attempt
from panel_dispatch import continuation
from panel_dispatch import moved_main
from panel_dispatch import scope_overlaps
from panel_dispatch import dispatch
from panel_dispatch import _validate_dispatch_verifier
from panel_dispatch import _read_suite
from panel_dispatch import _uses_smoke_runner
from panel_dispatch import plan_link
from panel_dispatch import _read_dispatch_brief
from panel_dispatch import continuation_defaults
from panel_dispatch import _validate_dry_run_inputs
from panel_review import _safe_patch_path
from panel_review import _AUTO_REVIEW_FRONTIER_PATHS
from panel_review import _automatic_review_workload
from panel_review import _call_result_event
from panel_review import _judgment_mail
from panel_review import _review_verdict
from panel_review import _review_dispatch_context
from panel_review import _rework_history
from panel_review import _write_brief_artifact
from panel_review import _review_findings
from panel_review import _review_fix_notice
from panel_review import _dispatch_execution_repository
from panel_review import _completion_inputs
from panel_review import _review_verifier
from panel_review import _settlement_claim_path
from panel_review import _settlement_owner_identity
from panel_review import SETTLEMENTS_HELD, SETTLEMENTS_HELD_LOCK
from panel_review import _settlement_claim
from panel_review import _settlement_owner_gone
from panel_review import _settlement_step
from panel_review import _settlement_release
from panel_review import _settlement_delivery_pending
from panel_review import _review_launch_state
from panel_review import deliver_result
from panel_review import _deliver_result_steps
from panel_dispatch import _rollover_if_needed
from panel_dispatch import _adopt_successor
from panel_dispatch import _dispatch_capacity
from panel_dispatch import _dispatch_group_gone
from panel_dispatch import _dispatch_exited
from panel_dispatch import _dispatch_leader_gone
from panel_dispatch import _dispatch_process_snapshot
from panel_dispatch import _dispatch_session_gone
from panel_dispatch import _dispatch_descendants
from panel_dispatch import _observe_dispatch_descendants
from panel_dispatch import _dispatch_descendants_gone
from panel_dispatch import _dispatch_recorded_gone
from panel_dispatch import _dispatch_call_path
from panel_dispatch import _dispatch_pending_path
from panel_dispatch import _dispatch_exit_record
from panel_dispatch import _dispatch_output_log
from panel_dispatch import _tee_dispatch_output
from panel_dispatch import _save_dispatch_output
from panel_dispatch import _dispatch_verifier
from panel_dispatch import _prepare_dispatch_call
from panel_dispatch import _valid_rework_context
from panel_dispatch import _dispatch_call_record
from panel_dispatch import _record_dispatch_pending
from panel_dispatch import _dispatch_pending
from panel_dispatch import _dispatch_supervisor_snapshot
from panel_dispatch import _dispatch_supervisor_receipt
from panel_dispatch import _cancel_watch_authority
from panel_navigation import _cancel_window_id
from panel_navigation import _cancel_client_name
from panel_navigation import _clear_cancel_option
from panel_navigation import _begin_cancel_confirmation
from panel_navigation import _cancel_option_value
from panel_navigation import _poll_cancel_confirmation
from panel_navigation import _request_cancel_confirmation
from panel_dispatch import cancel_selected_call
from panel_dispatch import _dispatch_call_finished
from panel_dispatch import _dispatch_released
from panel_dispatch import _reconcile_dispatch
from panel_dispatch import _dispatch_before_force
from panel_dispatch import _observe_dispatch_session
from panel_dispatch import _stop_dispatch_groups
from panel_dispatch import _stop_dispatch
from panel_dispatch import _settle_dispatch
from panel_dispatch import _rollback_dispatch_enrollment
from panel_dispatch import run_dispatch
from panel_dispatch import _print_dispatch_next_ready
from panel_dispatch import council_command
from panel_dispatch import council
from panel_dispatch import peer_command
from panel_dispatch import run
from panel_dispatch import dashboard
from panel_state import tmux_command
from panel_layout import shell_command
from panel_layout import panel_shell
from panel_layout import watch_shell
from panel_layout import panel_environment
from panel_layout import board_view
from panel_layout import reader_shell
from panel_layout import BOARD_SHARE
from panel_layout import BOARD_HEIGHT_SHARE
from panel_layout import OVERVIEW_SHARE
from panel_layout import MIN_BOARD, MIN_CHAT
from panel_layout import WORK_CHAT_RESERVE
from panel_layout import PANEL_MODES
from panel_layout import parse_split
from panel_layout import parse_work_drag
from panel_layout import split_text
from panel_layout import top_text
from panel_layout import split_cells
from panel_layout import NATIVE_MIN
from panel_layout import board_split_cells
from panel_layout import work_split_cells
from panel_layout import next_panel_mode
from panel_layout import cycle_panel_mode
from panel_layout import terminal_window
from panel_layout import board_split
from panel_layout import panel_geometry
from panel_layout import PanelModeSync
from panel_layout import SplitSync
from panel_layout import source_stamps
from panel_layout import REJECTED_RELOAD
from panel_layout import RELOAD_MODULES
from panel_layout import reload_ready
from panel_layout import panel_panes
from panel_layout import reopen_panel
from panel_state import session_candidates
from panel_state import resolve_session
from panel_state import session_owner
from panel_state import room_open
from panel_state import room_headroom
from panel_state import recorded_room
from panel_state import session_room
from panel_state import panel_room
from panel_native import launch_command
from panel_layout import bind_window
from panel_layout import keys_help


KEYS_HELP = keys_help()
# Text inside tmux's conditional cannot contain commas or closing braces.
MAIN_KEYS = (("F9", "select-pane -t :.+"),
             ("F5", 'if-shell -F "#{==:#{window_name},control}" "select-window -l" "select-window -t :=control"'),
             ("F8", 'if-shell -F "#{==:#{@oms_panel_mode},overview}" "set-option -w @oms_panel_mode focus" '
                    '"if-shell -F \\"#{==:#{@oms_panel_mode},focus}\\" \\"set-option -w @oms_panel_mode work\\" '
                    '\\"set-option -w @oms_panel_mode overview\\""'),
             ("F12", 'display-message -d 8000 "#{?#{==:#{window_name},control},%s,%s}"' % KEYS_HELP))
from panel_layout import bind_main_keys
from panel_native import legacy_session
from panel_native import panel_mains
from panel_native import add_main_window
from panel_native import MAX_LIVE_MAINS
from panel_native import live_mains
from panel_native import live_main_refusal
from panel_native import check_legacy_main
from panel_native import spawn_main
from panel_native import swap_choices
from panel_native import _swap_main_window
from panel_native import _swap_digest
from panel_native import _latest_swap
from panel_native import _swap_handoff_file
from panel_native import _swap_submit
from panel_native import swap_main
from panel_native import spawn_single_main
from panel_native import split_panel
from panel_native import construct_panel
from panel_native import open_native
from panel_interaction import read_field
from panel_render import terminal_style
from panel_render import managed_session
from panel_state import MAIN_OUTPUT
from panel_state import MAIN_INTERRUPT_CACHE
from panel_state import _interrupted_transcript
from panel_state import main_activity
from panel_state import snapshot
from panel_state import refresh_failed
from panel_state import hold_state
from panel_render import stale_notice
from panel_render import AVAILABLE
from panel_render import provider_availability
from panel_render import frame_view
from panel_state import awaiting_admission
from panel_render import _show_state
from panel_render import show
from panel_render import draw_frame
from panel_navigation import session_option
from panel_navigation import pane_option
from panel_navigation import pane_local_option
from panel_navigation import reset_group_view
from panel_navigation import navigate
from panel_navigation import read_evidence
from panel_navigation import finish_opening
from panel_navigation import expire_notice
from panel_navigation import window_zoomed
from panel_navigation import call_results
from panel_navigation import resolve_call_alias
from panel_navigation import render_call_view
from panel_navigation import advisor_command
from panel_navigation import ask_advisor
from panel_navigation import worker_worktree
from panel_navigation import open_worktree
from panel_navigation import tab_event
from panel_navigation import BOTTOM_OPTION
from panel_navigation import BOTTOM_LIMIT
from panel_navigation import BOTTOM_KINDS
from panel_navigation import HEADER_OPTION
from panel_navigation import HEADER_ENABLED
from panel_navigation import _header_safe
from panel_navigation import context_header_text
from panel_navigation import _tmux_output_value
from panel_navigation import _window_option_local
from panel_navigation import update_context_header
from panel_navigation import toggle_context_headers
from panel_navigation import bottom_state
from panel_navigation import bottom_text
from panel_navigation import read_bottom
from panel_navigation import apply_bottom
from panel_navigation import publish_bottom
from panel_navigation import refreshed_navigation
from panel_watch import BackgroundRead
from panel_state import read_panel
from panel_state import private_capacity
from panel_state import WINDOW_ATTENTION
from panel_state import main_needs_attention
from panel_state import main_has_open_questions
from panel_watch import mark_window
from panel_watch import start_spawn
from panel_watch import reap_spawns
from panel_watch import watch
from panel_interaction import browse_results
from panel_interaction import browse_chats
from panel_interaction import raw_keys
from panel_interaction import menu_input
from panel_interaction import read_choice
from panel_review import finalize_selection
from panel_interaction import interactive


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
    parser.add_argument("--launch", help="open the sole main (claude preferred, pi or codex)")
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
    parser.add_argument("--metrics", action="store_true",
                        help="read-only recorded metrics; follow-up task IDs have no producer linkage")
    parser.add_argument("--since", metavar="DURATION", help="limit --metrics to records within a duration (for example 7d)")
    parser.add_argument("--json", action="store_true", help="read-only availability and shared OMS state")
    parser.add_argument("--watch", action="store_true", help="read-only activity tree, refreshed every five seconds")
    parser.add_argument("--owner", help="main identity for --watch or --dispatch")
    parser.add_argument("--count", type=int, help="stop after N watch snapshots")
    parser.add_argument("--no-animation", action="store_true", help="keep --watch motion-free")
    parser.add_argument("--view", choices=("auto", "summary", "compact", "detail", "graph", "tree"), help="activity density for the menu or --watch")
    parser.add_argument("--attention-only", action="store_true", help="show only calls awaiting action in the menu or --watch")
    parser.add_argument("--routes", action="store_true", help="show the panel role policy without calling models")
    parser.add_argument("--task", help="task for the native main (requires --launch)")
    parser.add_argument("--dispatch", choices=tuple(KINDS), metavar="ROLE", help="run one bounded role task: " + ", ".join(KINDS))
    parser.add_argument("--main", help="assert the dispatch caller's own main participant (never select another main)")
    parser.add_argument("--scope", action="append", metavar="PATH",
                        help="repeatable repo-relative scope a write worker declares; overlaps warn, never lock")
    parser.add_argument("--council", action="store_true", help="run the two-family debate (deep tier by default)")
    parser.add_argument("--rounds", type=int, default=1, help="council debate rounds, 1 or 2")
    parser.add_argument("--thread", help="council thread ID")
    parser.add_argument("--results", action="store_true", help="read bounded durable task results")
    parser.add_argument("--salvage", metavar="TASK_ID", help="capture a stopped worker's kept worktree for owner review")
    parser.add_argument("--mail", metavar="PARTICIPANT", help="read messages addressed to one main in --room")
    parser.add_argument("--participant", help="--results of one room call (with --task-id, one council seat)")
    parser.add_argument("--chats", action="store_true", help="read enrolled native chats and navigation capabilities")
    parser.add_argument("--open-chat", help="navigate to a selected room participant without starting a model turn")
    parser.add_argument("--chat-surface", choices=("auto", "terminal", "app"), default="auto")
    parser.add_argument("--accept", metavar="TASK", help="commit one reviewed worker candidate locally; publication stays explicit")
    parser.add_argument("--message", help="commit message for --accept")
    parser.add_argument("--expected-patch-sha256", help="assert the reviewed candidate digest for --accept")
    parser.add_argument("--landed-commit", help="finish the plan with a published SHA before --finalize")
    parser.add_argument("--finalize", action="store_true", help="record an owner result")
    parser.add_argument("--settle-review", action="store_true", help="list stopped review workers; --apply closes eligible attempts")
    parser.add_argument("--apply", action="store_true", help="apply --settle-review (default: dry run)")
    parser.add_argument("--owner-attempt", help="--settle-review for a named dead main's lineage")
    parser.add_argument("--legacy", action="store_true", help="allow aged terminal mains with no host/native ownership refs")
    parser.add_argument("--legacy-older-than", type=float, metavar="HOURS", help="legacy main lifecycle age (default: 24 hours)")
    parser.add_argument("--standalone", action="store_true", help="settle aged parentless review workers")
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
    parser.add_argument("--result-id", help="saved result revision for --retry-delivery")
    parser.add_argument("--workload", choices=TIERS,
                        help="capability tier instead of the role's default; council accepts deep or frontier")
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
            args.scope = args.scope or inherited["scope"] or None
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
    if args.since is not None and not args.metrics:
        parser.error("--since requires --metrics")
    if args.alias is not None and not args.dispatch:
        parser.error("--alias requires --dispatch")
    if sum(bool(value) for value in (args.watch, args.launch, args.spawn_main, args.swap_main, args.reopen, args.dispatch, args.routes,
                                     args.council, args.results, args.accept, args.finalize, args.retry_delivery, args.providers,
                                     args.chats, args.open_chat, args.health, args.mail, args.metrics)) > 1:
        parser.error("choose one panel operation")
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
    if (args.rounds != 1 or args.thread) and not args.council:
        parser.error("--rounds and --thread require --council")
    if (args.summary_file or args.outcome or args.evidence or args.notify or args.verify_timeout is not None) and not args.finalize:
        parser.error("finalization options require --finalize")
    if args.retry_delivery and not args.task_id:
        parser.error("--retry-delivery requires --task-id")
    if args.result_id and not args.retry_delivery:
        parser.error("--result-id requires --retry-delivery")
    if args.dry_run and (args.results or args.accept or args.finalize or args.retry_delivery or args.providers or args.chats):
        parser.error("--dry-run only plans a launch, role dispatch or council")
    if (args.message is not None or args.expected_patch_sha256 is not None) and not args.accept:
        parser.error("--message and --expected-patch-sha256 require --accept")
    if args.accept is not None and (not args.accept or not (args.repo and args.message and args.verify) or args.task_id):
        parser.error("--accept TASK requires --repo, --message and --verify; takes no --task-id")
    if args.landed_commit is not None and not args.finalize:
        parser.error("--landed-commit requires --finalize")
    if args.finalize and not (args.task_id and args.summary_file and args.outcome and args.owner):
        parser.error("--finalize requires --owner, --task-id, --summary-file and --outcome")
    if args.landed_commit is not None and args.outcome not in ("accepted", "completed"):
        parser.error("--landed-commit requires --outcome accepted or completed")
    if args.council and not (args.task_id and args.label and args.prompt):
        parser.error("--council requires --task-id, --label and --prompt")
    if (args.to or args.reasoning_effort) and not args.dispatch:
        parser.error("--to and --reasoning-effort require --dispatch")
    if args.council and args.workload and args.workload not in ("deep", "frontier"):
        parser.error("council --workload must be deep or frontier")
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
            args.json or args.dry_run or args.watch or args.routes or args.results or args.providers or args.health or args.mail or args.metrics)) and (
            os.environ.get("OMS_HARNESS_CHILD") == "1"
            or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"):
        parser.error("a worker cannot open owner sessions or orchestrate peers; return the need to its parent")
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
            chosen = {}
            for role in routes["roles"]:
                try:
                    route = allocate(owner, role, installed=installed, provider_status=readings, recent=recent)
                    peer = next(row for row in rows if row["provider"] == route["provider"])
                    route["model"] = newest_route_model(route["provider"], route["model"],
                        peer.get("catalog_models", peer.get("models", [])))
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
            if room.status(repo, args.room)["closed"] and not (args.chats or args.open_chat or args.health):
                raise ValueError("selected room is closed")
            os.environ["OMS_ROOM_ID"] = args.room
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
        if args.metrics:
            from panel_metrics import aggregate, metrics_text
            report = aggregate(repo, args.since)
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
        if args.launch and args.launch not in MAIN_HARNESSES:
            args.launch = provider_catalog(args.launch)[0]["provider"]
            native_binary(args.launch)
        if args.launch == "pi" and args.resume:
            raise ValueError("Pi resume needs a verified Pi session binding; start a new Pi main")
        if args.spawn_main:
            if args.spawn_main not in MAIN_HARNESSES:
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
            return council(repo, args.owner or caller_owner(), args.task_id, args.label, args.prompt,
                           args.thread, args.rounds, args.dry_run, args.workload or "deep")
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
                read = results(repo, args.task_id, participant)
                if participant:
                    read["rows"] = [row for row in read["rows"] if row.get("calls")]
                return read

            board = os.environ.get(panel_reader.BOARD, "")
            if args.json:
                print(json.dumps(report(), ensure_ascii=False, indent=2))
            elif panel_reader.PANE.fullmatch(board) and sys.stdin.isatty() and sys.stdout.isatty():
                return panel_reader.page(reader, board, on_refresh=reader_cache.clear)
            else:
                print(reader(shutil.get_terminal_size().columns))
            return 0
        if args.accept:
            report = accept(repo, args.owner or caller_owner(), args.accept, args.message, args.verify,
                            args.expected_patch_sha256, execution_repo=execution)
            print("committed locally; publication pending: " + report['sha'])
            print("Next: oms land --repo " + shlex.quote(str(execution)))
            return 0
        if args.finalize:
            report = finalize(repo, args.owner, args.task_id, args.summary_file, args.outcome,
                              args.verify, args.evidence, args.notify,
                              1800 if args.verify_timeout is None else args.verify_timeout,
                              execution_repo=execution, landed_commit=args.landed_commit)
            print(json.dumps(report, ensure_ascii=False, indent=2))
            return 0
        if args.retry_delivery:
            print(json.dumps(retry_delivery(repo, args.task_id, args.result_id), ensure_ascii=False, indent=2))
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
                              continue_task=args.continue_task, scopes=args.scope, work_repo=execution, alias=args.alias,
                              allow_stale_base=args.allow_stale_base, from_task=args.from_task)
            if asked:
                read_field("Exit %s; the answer is in the main's room mail. Press Enter to close: " % status,
                           required=False)
            return status
        if args.watch:
            provider = args.owner or caller_owner()
            if provider not in PROVIDERS:
                provider = provider_catalog(provider)[0]["provider"]
            return watch(repo, provider, args.count or 0, args.no_animation,
                         args.view, args.attention_only)
        if args.json:
            state, status = snapshot(repo)
            print(json.dumps({"schema": 1, "kind": "oms-panel", "dashboard": state,
                              "providers": {name: {"installed": bool(shutil.which(name))}
                                            for name in PROVIDERS},
                              "main_harnesses": {name: {"installed": main_installed(name), "owner": main_owner(name)}
                                                 for name in MAIN_HARNESSES},
                              "max_live_mains": MAX_LIVE_MAINS,
                              "split_available": bool(shutil.which("tmux")),
                              "routing": policy(),
                              "coverage": "OMS records; native sessions are separate"}))
            return status
        # The checkout's fixed tmux panel is the default home, also from inside another tmux
        # session (switch-client, no nesting) and for --launch; inline stays an explicit choice.
        split = args.host not in ("inline", "herdr") and not panel_host.herdr_active() and (
            args.host == "tmux" or args.layout == "split" or (
                args.layout == "auto" and shutil.which("tmux") and os.name != "nt"))
        if args.dry_run:
            print(json.dumps({"schema": 1, "kind": "oms-panel-launch",
                              "repo": str(repo), "layout": "split" if split else "inline",
                              "host": "tmux" if split else "herdr" if args.host == "herdr" or panel_host.herdr_active() else "inline",
                              "provider": main_owner(args.launch),
                              "harness": args.launch,
                              "max_live_mains": MAX_LIVE_MAINS,
                              "argv": native_command(args.launch, repo, args.resume, args.model, args.task)
                              if args.launch else None,
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
        return interactive(repo, args.view or "auto", args.attention_only)
    except ReloadWatcher:
        # The watcher restored the terminal; re-execute it on the updated, compiling sources.
        os.execv(sys.executable, [sys.executable] + sys.argv)
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
        else:
            message = "error: " + clean(str(error), 200)
        print(clean(message, 207), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
