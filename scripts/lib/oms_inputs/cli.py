"""The public `oms input` interface; no pane injection or model invocation."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from oms_runtime.common import CoreError
from . import attachments, feedback, queue
from .common import child, context, digest, item, parent_only, read_owned, safe_text, state


def parser():
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument("--repo", default=argparse.SUPPRESS)
    shared.add_argument("--json", action="store_true", default=argparse.SUPPRESS, help="JSON output (default)")
    root = argparse.ArgumentParser(prog="oms input", parents=[shared],
        description="Capture source-aware attachments and edit local steer/next inputs before explicit room delivery.")
    commands = root.add_subparsers(dest="action", required=True)
    attach = commands.add_parser("attach", parents=[shared])
    kinds = attach.add_subparsers(dest="attachment_kind", required=True)
    file = kinds.add_parser("file", parents=[shared])
    file.add_argument("path")
    file.add_argument("--start-line", type=int)
    file.add_argument("--end-line", type=int)
    selection = kinds.add_parser("selection", parents=[shared])
    selection.add_argument("--source", choices=("browser", "chat", "text"), required=True)
    selection.add_argument("--origin", required=True)
    terminal = kinds.add_parser("terminal", parents=[shared])
    terminal.add_argument("--pane", required=True)
    terminal.add_argument("--lines", type=int, default=80)

    def text_args(p, required=True):
        group = p.add_mutually_exclusive_group(required=required)
        group.add_argument("--text")
        group.add_argument("--text-file")

    def route_args(p):
        p.add_argument("--room", required=True)
        p.add_argument("--from", dest="sender", required=True)
        p.add_argument("--to", required=True)
        p.add_argument("--mode", choices=("steer", "next"), default="next")

    text_args(selection)
    one = commands.add_parser("attachment", parents=[shared])
    one.add_argument("id")
    one.add_argument("--full", action="store_true", help="return the full bounded snapshot instead of its first 8 KiB")
    commands.add_parser("attachments", parents=[shared])
    add = commands.add_parser("add", parents=[shared])
    route_args(add)
    text_args(add)
    add.add_argument("--attachment", action="append", default=[])
    edit = commands.add_parser("edit", parents=[shared])
    edit.add_argument("id")
    text_args(edit, False)
    edit.add_argument("--mode", choices=("steer", "next"))
    edit.add_argument("--attachment", action="append", help="replace the entire attachment list")
    edit.add_argument("--clear-attachments", action="store_true")
    move = commands.add_parser("move", parents=[shared])
    move.add_argument("id")
    move.add_argument("--before", help="queued item ID; omit to move to the end")
    commands.add_parser("list", parents=[shared])
    for action in ("show", "preview", "cancel", "deliver", "reconcile"):
        p = commands.add_parser(action, parents=[shared])
        p.add_argument("id")
        if action == "deliver":
            p.add_argument("--apply", action="store_true", help="persist a message; otherwise preview only")
            p.add_argument("--at-boundary", action="store_true", help="explicit next-task boundary assertion")
            p.add_argument("--expected-sha256", help="require this exact previewed payload")
    adapter = commands.add_parser("feedback", parents=[shared])
    adapter.add_argument("--batch", required=True, help="exact imported artifact-view feedback batch SHA-256")
    route_args(adapter)
    return root


def _text(args, limit):
    if getattr(args, "text_file", None):
        return safe_text(read_owned(Path(args.text_file).expanduser().absolute(), limit).decode("utf-8"), limit)
    raw = getattr(args, "text", None)
    return safe_text(raw, limit) if raw is not None else None


def run(args):
    read_only = args.action in {"attachment", "attachments", "list", "show", "preview"} or args.action == "deliver" and not args.apply
    if child() and args.action != "attachment":
        raise CoreError("workers may retrieve only explicitly addressed attachment IDs; queue inspection is parent-only")
    if not read_only:
        parent_only()
    default = os.environ.get("OMS_ROOM_REPO", os.getcwd()) if read_only and child() else os.getcwd()
    work, repo = context(getattr(args, "repo", default))
    if args.action == "attach":
        if args.attachment_kind == "file":
            return attachments.attach_file(repo, work, args.path, args.start_line, args.end_line)
        if args.attachment_kind == "selection":
            return attachments.attach_selection(repo, args.source, args.origin, _text(args, attachments.MAX_ATTACHMENT))
        return attachments.attach_terminal(repo, work, args.pane, args.lines)
    if args.action == "attachment":
        if child():
            queue.authorize_child_attachment(repo, args.id)
        return attachments.retrieve(repo, args.id, args.full)
    if args.action == "attachments":
        return {"schema": 1, "attachments": attachments.listing(repo)}
    if args.action == "list":
        value = state(repo)
        return {"schema": 1, "revision": value["revision"], "items": value["items"]}
    if args.action == "add":
        return queue.add(repo, work, args.room, args.sender, args.to, _text(args, 3500), args.mode, args.attachment)
    if args.action == "edit":
        if args.clear_attachments and args.attachment:
            raise CoreError("choose replacement attachments or --clear-attachments")
        return queue.edit(repo, args.id, _text(args, 3500), args.mode, [] if args.clear_attachments else args.attachment)
    if args.action == "move":
        return queue.move(repo, args.id, args.before)
    if args.action == "cancel":
        return queue.cancel(repo, args.id)
    if args.action == "show":
        return item(state(repo), args.id)
    if args.action == "preview":
        return queue.preview(repo, item(state(repo), args.id))
    if args.action == "deliver":
        return queue.deliver(repo, args.id, args.apply, args.at_boundary,
                             digest(args.expected_sha256) if args.expected_sha256 else None)
    if args.action == "reconcile":
        return queue.reconcile(repo, args.id)
    if args.action == "feedback":
        return feedback.enqueue(repo, work, args.batch, args.room, args.sender, args.to, args.mode)
    raise CoreError("unsupported input action")


def main(argv=None):
    try:
        # Fail closed rather than silently interpreting --repo through an
        # inherited alternate Git repository or index, including in bridges.
        if any(os.environ.get(key) for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR",
                "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE")):
            raise CoreError("input refuses Git repository/index redirect environment variables; select a clean --repo context")
        report = run(parser().parse_args(argv))
        print(json.dumps(report, ensure_ascii=True, sort_keys=True, indent=2))
        return 3 if report.get("status") == "uncertain" else 0
    except (CoreError, OSError, ValueError, TypeError, KeyError, AttributeError, RecursionError,
            subprocess.SubprocessError) as exc:
        detail = str(exc) if isinstance(exc, CoreError) else "invalid, unavailable, or unsafe local input state"
        print("error: " + detail, file=sys.stderr)
        return getattr(exc, "exit_code", 2)


if __name__ == "__main__":
    raise SystemExit(main())
