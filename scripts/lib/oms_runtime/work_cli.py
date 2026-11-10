"""CLI wiring for read-only matrix, next-action explanations and return briefs."""
from __future__ import annotations

from .common import CoreError
from .work_inspect import WorkViewError, action_inbox, evidence_matrix, render, return_brief


def dispatch(args, repo, build_envelope, emit):
    """Return None for existing actions, otherwise execute a bounded read query."""
    matrix = args.command == "evidence" and args.evidence_action == "matrix"
    brief = args.command == "brief"
    explain = args.command == "next" and args.explain
    if args.command == "next" and not explain:
        if args.task is not None or args.limit != 20 or args.format != "json":
            raise CoreError("next --task/--limit/--format require --explain")
    if not (matrix or brief or explain):
        return None
    if not 1 <= args.limit <= 100:
        raise CoreError("limit must be from 1 to 100")
    if args.format == "text" and args.pretty:
        raise CoreError("--pretty applies to JSON, not --format text")
    try:
        previous = None
        if brief and args.before is not None:
            # Reuse the descriptor-relative bounded JSON reader. It validates
            # only the read boundary; work_inspect validates schema 2 below.
            from .context_inspect import InspectionError, read_manifest
            try:
                previous = read_manifest(repo, args.before)
            except InspectionError:
                raise WorkViewError("cannot safely read the selected prior envelope") from None
            # Accept the envelope-write acknowledgement as well as the bare
            # envelope. Never follow its 'written' field as another path.
            if isinstance(previous, dict) and "envelope" in previous:
                if set(previous) != {"written", "envelope"}:
                    raise WorkViewError("invalid envelope-write acknowledgement")
                previous = previous["envelope"]
            from .work_inspect import _validate
            _validate(previous)
        current = build_envelope(repo)
        if matrix:
            report = evidence_matrix(current, limit=args.limit, task_id=args.task)
        elif explain:
            report = action_inbox(current, limit=args.limit, task_id=args.task)
        else:
            report = return_brief(current, previous, limit=args.limit)
    except WorkViewError as exc:
        raise CoreError(str(exc)) from None
    if args.format == "text":
        print(render(report), end="")
    else:
        emit(report, args.pretty)
    return 0
