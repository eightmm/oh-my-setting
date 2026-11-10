"""The `oms session` interface; JSON reports remain local, never a provider prompt."""
from __future__ import annotations

import argparse
import json
import os
import sys

from oms_runtime.common import CoreError
from . import native, transcripts, worktrees
from .common import label, load, parent_only, records, repository


def parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--repo", default=argparse.SUPPRESS, help="Git repository (default: current directory)")
    common.add_argument("--json", action="store_true", default=argparse.SUPPRESS, help="emit JSON (default)")
    root = argparse.ArgumentParser(prog="oms session", parents=[common],
        description="Search local native sessions; fork conversation and code; restore files or conversation independently.",
        epilog="Native launches and file restores are dry runs unless --apply is explicit. No cloud search or transcript editing.")
    actions = root.add_subparsers(dest="action", required=True)

    def source(p):
        p.add_argument("--provider", choices=transcripts.PROVIDERS, required=True)
        p.add_argument("--session", required=True, help="exact native session ID")
        p.add_argument("--root", action="append", default=[], help="explicit bounded transcript directory; repeatable")

    search = actions.add_parser("search", parents=[common], help="literal search of repository-bound native messages")
    search.add_argument("query")
    search.add_argument("--provider", choices=("all", *transcripts.PROVIDERS), default="all")
    search.add_argument("--root", action="append", default=[], help="directory, or PROVIDER=PATH for cross-provider roots")
    search.add_argument("--limit", type=int, default=20)
    opened = actions.add_parser("open", parents=[common], help="read bounded excerpts with exact native origins")
    source(opened)
    opened.add_argument("--line", type=int, default=1)
    opened.add_argument("--limit", type=int, default=20)
    resume = actions.add_parser("resume", parents=[common], help="plan or launch the same native session")
    source(resume)
    resume.add_argument("--apply", action="store_true")
    fork = actions.add_parser("fork", parents=[common], help="fork the conversation in an independent copy of current files")
    source(fork)
    fork.add_argument("--name", default="")
    fork.add_argument("--apply", action="store_true")
    fork.add_argument("--no-launch", action="store_true", help="prepare files only; explicitly launch later")
    fork.add_argument("--owner-pid", type=int, help="live session process that owns the scratch registration")
    launch = actions.add_parser("launch", parents=[common], help="launch a prepared conversation fork once")
    launch.add_argument("id")
    launch.add_argument("--apply", action="store_true")
    actions.add_parser("forks", parents=[common], help="read local lineage and native launch outcomes")
    comparison = actions.add_parser("compare", parents=[common], help="compare two fork worktrees and artifact hashes")
    comparison.add_argument("left")
    comparison.add_argument("right")
    comparison.add_argument("--patch", action="store_true", help="include bounded text diffs; secret-like text is withheld")

    bookmark = actions.add_parser("bookmark", parents=[common], help="save or reopen exact local origins without copying text")
    book_actions = bookmark.add_subparsers(dest="bookmark_action", required=True)
    add = book_actions.add_parser("add", parents=[common])
    source(add)
    add.add_argument("--line", type=int, required=True)
    add.add_argument("--part", type=int, default=0)
    add.add_argument("--label", default="")
    book_actions.add_parser("list", parents=[common])
    for name in ("open", "resume"):
        p = book_actions.add_parser(name, parents=[common])
        p.add_argument("id")
        if name == "resume":
            p.add_argument("--apply", action="store_true")

    restore = actions.add_parser("restore", parents=[common], help="choose file restore or conversation fork explicitly")
    restore_actions = restore.add_subparsers(dest="restore_action", required=True)
    files = restore_actions.add_parser("files", parents=[common], help="existing checkpoint restore; same HEAD required")
    files.add_argument("checkpoint")
    files.add_argument("--apply", action="store_true")
    files.add_argument("--untracked-only", action="store_true")
    conversation = restore_actions.add_parser("conversation", parents=[common], help="native fork in current checkout; no OMS file restore")
    source(conversation)
    conversation.add_argument("--apply", action="store_true")
    return root


def run(args):
    parent_only()
    repo = repository(getattr(args, "repo", os.getcwd()))
    if hasattr(args, "limit") and not 1 <= args.limit <= 100:
        raise CoreError("--limit must be between 1 and 100")
    if hasattr(args, "line") and args.line < 1 or hasattr(args, "part") and args.part < 0:
        raise CoreError("line numbers start at 1 and part indices at 0")
    if args.action == "search":
        return transcripts.search(repo, args.query, args.provider, args.root, args.limit)
    if args.action == "forks":
        return {"schema": 1, "kind": "oms-session-forks", "forks": records(repo, "forks")}
    if args.action == "compare":
        return worktrees.compare(repo, args.left, args.right, args.patch)
    if args.action == "launch":
        return native.launch_fork(repo, load(repo, "forks", args.id), args.apply)
    if args.action == "restore" and args.restore_action == "files":
        return worktrees.restore_files(repo, args.checkpoint, args.apply, args.untracked_only)
    if args.action == "bookmark":
        if args.bookmark_action == "list":
            return {"schema": 1, "kind": "oms-session-bookmarks", "bookmarks": records(repo, "bookmarks")}
        if args.bookmark_action in ("open", "resume"):
            source, origin = transcripts.bookmarked(repo, args.id)
            if args.bookmark_action == "open":
                return {"schema": 1, "kind": "oms-session-bookmark-open", "origin": origin}
            return native.conversation(repo, source, apply=args.apply)
    source = transcripts.select(repo, args.provider, args.session, args.root)
    if args.action == "bookmark":
        return transcripts.bookmark(repo, source, args.line, args.part, args.label)
    if args.action == "open":
        return transcripts.show(source, args.line, args.limit)
    if args.action == "resume":
        return native.conversation(repo, source, apply=args.apply)
    if args.action == "restore":
        return native.conversation(repo, source, fork=True, apply=args.apply)
    if args.action == "fork":
        label(args.name)
        # Verify the executable capability before creating any checkpoint or
        # registration, including for an intentionally prepared-only fork.
        request = native.plan(source, repo, fork=True)
        files = worktrees.inspect_files(repo)
        if not args.apply:
            return {"schema": 1, "kind": "oms-session-fork-plan", "source": source, "files": files,
                    "native": dict(request, cwd="<new scratch worktree>",
                                   argv=["<new scratch worktree>" if arg == str(repo) else arg for arg in request["argv"]]),
                    "dry_run": True,
                    "note": "Current file state is copied. Historical conversation turns do not imply historical file state."}
        row = worktrees.create(repo, source, args.name, args.owner_pid)
        if args.no_launch:
            return dict(row, dry_run=False)
        return native.launch_fork(repo, row, True)
    raise CoreError("unsupported session action")


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        report = run(args)
        print(json.dumps(report, ensure_ascii=True, sort_keys=True, indent=2))
        returncode = report.get("native_returncode", 0)
        return returncode if 0 <= returncode <= 255 else 1
    except (CoreError, OSError, ValueError, KeyError, TypeError) as exc:
        # JSON parser errors can quote input text. Do not expose native data
        # from malformed transcripts or corrupt local records in diagnostics.
        message = str(exc) if isinstance(exc, CoreError) else "invalid, unsafe, or unavailable local session state"
        print("error: " + message, file=sys.stderr)
        return getattr(exc, "exit_code", 2)


if __name__ == "__main__":
    raise SystemExit(main())
