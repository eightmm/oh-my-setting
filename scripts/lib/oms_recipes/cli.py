"""CLI for explicitly invoked recipes and advisory local workflow requests."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import sys

from oms_runtime.common import CoreError, read_jsonl, repo_root, utc_now
from . import catalog, definitions, runner, triggers
from .common import (
    MAX_DEFINITION_BYTES, child, identifier, load_definition, records,
    state_root, under,
)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="oms recipe", description="Explicit local recipes, workspace actions, packs, worksets and due/event requests.")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--json", action="store_true", help="JSON is the default output")
    commands = parser.add_subparsers(dest="command", required=True)

    def leaf(group, name, **kwargs):
        result = group.add_parser(name, **kwargs)
        result.add_argument("--repo", default=argparse.SUPPRESS)
        result.add_argument("--json", action="store_true", default=argparse.SUPPRESS)
        return result

    def invocation(p, workspace=False):
        p.add_argument("file")
        p.add_argument("--input", action="append", default=[], metavar="NAME=VALUE")
        if workspace:
            p.add_argument("workspace_action")
            p.add_argument("--port", action="append", default=[], metavar="NAME=NUMBER")
            p.add_argument("--with", dest="companions", action="append", default=[], metavar="COMPANION")
        else:
            p.add_argument("--overlay", default="")

    validate = leaf(commands, "validate")
    validate.add_argument("file")
    validate.add_argument("--overlay", default="")
    for name in ("plan", "run"):
        p = leaf(commands, name)
        invocation(p)
        if name == "run":
            p.add_argument("--execute", action="store_true", help="explicitly authorize this invocation's host processes")
    p = leaf(commands, "resume")
    p.add_argument("id")
    p.add_argument("--file", required=True, help="re-read the original definition and verify its digest")
    p.add_argument("--overlay", default="")
    p.add_argument("--execute", action="store_true")
    p.add_argument("--retry-failed", action="store_true")
    p.add_argument("--retry-uncertain", action="store_true", help="acknowledge that interrupted local side effects may be replayed")
    p = leaf(commands, "show")
    p.add_argument("id")
    p = leaf(commands, "history")
    p.add_argument("--id", default="")
    p.add_argument("--limit", type=int, default=100)
    workspace = commands.add_parser("workspace")
    actions = workspace.add_subparsers(dest="action", required=True)
    for name in ("plan", "run"):
        p = leaf(actions, name)
        invocation(p, workspace=True)
        if name == "run":
            p.add_argument("--execute", action="store_true")
    pack = commands.add_parser("pack")
    actions = pack.add_subparsers(dest="action", required=True)
    p = leaf(actions, "inspect")
    p.add_argument("directory")
    p = leaf(actions, "import")
    p.add_argument("directory")
    p.add_argument("--digest", required=True)
    p.add_argument("--resource", action="append", default=[])
    leaf(actions, "list")
    workset = commands.add_parser("workset")
    actions = workset.add_subparsers(dest="action", required=True)
    p = leaf(actions, "put")
    p.add_argument("file")
    p.add_argument("--replace", action="store_true")
    p = leaf(actions, "show")
    p.add_argument("id")
    leaf(actions, "list")
    trigger = commands.add_parser("trigger")
    actions = trigger.add_subparsers(dest="action", required=True)
    p = leaf(actions, "add")
    p.add_argument("file")
    for name in ("list", "requests"):
        leaf(actions, name)
    for name in ("enable", "disable", "cancel", "plan", "run"):
        p = leaf(actions, name)
        p.add_argument("id")
        if name == "run":
            p.add_argument("--execute", action="store_true")
            p.add_argument("--retry-failed", action="store_true")
            p.add_argument("--retry-uncertain", action="store_true")
    p = leaf(actions, "tick")
    p.add_argument("--now", default=None, help="UTC instant; defaults to current UTC")
    p = leaf(actions, "event")
    p.add_argument("name")
    p.add_argument("--event-id", required=True)
    p.add_argument("--data", default="{}", help="flat JSON object or local JSON file")
    return parser


def _pairs(items: list[str], *, ports: bool = False) -> dict:
    result = {}
    for item in items:
        name, separator, value = item.partition("=")
        identifier(name, "input/port name", name=True)
        if not separator or name in result:
            raise CoreError("inputs/ports require unique NAME=VALUE entries")
        try:
            result[name] = int(value) if ports else value
        except ValueError:
            raise CoreError("port must be an integer")
    return result


def _read_only(args) -> bool:
    if args.command in {"validate", "plan", "show", "history"}:
        return True
    return (args.command, getattr(args, "action", "")) in {
        ("workspace", "plan"), ("pack", "inspect"), ("pack", "list"),
        ("workset", "show"), ("workset", "list"),
        ("trigger", "list"), ("trigger", "requests"), ("trigger", "plan"),
    }


def _execute_requested(args) -> None:
    if not args.execute:
        raise CoreError("execution requires explicit --execute; use plan to inspect the invocation")


def _input_file(repo: Path, value: str) -> Path:
    path = Path(value).expanduser()
    return path if path.is_absolute() else under(repo, value)


def dispatch(args) -> tuple[object, int]:
    if (child() or os.environ.get("OMS_RECIPE_PROCESS") == "1") and not _read_only(args):
        raise CoreError("a harness child or recipe process may only use read-only recipe actions")
    repo = repo_root(args.repo)
    command = args.command
    if command == "validate":
        loaded = definitions.bundle(repo, args.file, args.overlay)
        result = {key: loaded[key] for key in ("definition_digest", "overlay_digest", "effective_digest", "source")}
        result.update(schema=1, id=loaded["definition"]["id"], kind=loaded["definition"]["kind"], valid=True)
        return result, 0
    if command in {"plan", "run", "workspace"}:
        workspace = command == "workspace"
        loaded = definitions.bundle(repo, args.file, "" if workspace else args.overlay)
        if (loaded["definition"]["kind"] == "workspace") != workspace:
            raise CoreError("workspace definitions use workspace plan/run; recipes use plan/run")
        inputs = definitions.bind_inputs(loaded["definition"].get("inputs", {}), _pairs(args.input), cli=True)
        plan = definitions.compile_plan(repo, loaded, inputs, action=args.workspace_action if workspace else "",
                                        ports=_pairs(args.port, ports=True) if workspace else {},
                                        companions=args.companions if workspace else [])
        if (args.action if workspace else command) == "plan":
            return definitions.public_plan(plan), 0
        _execute_requested(args)
        return runner.execute(repo, plan)
    if command == "resume":
        _execute_requested(args)
        previous = runner.load_run(repo, args.id)
        loaded = definitions.bundle(repo, args.file, args.overlay)
        plan = definitions.compile_plan(repo, loaded, previous["inputs"], action=previous["action"],
                                        ports=previous["ports"], companions=previous["companions"])
        return runner.execute(repo, plan, run_id=args.id, resume=True,
                              retry_failed=args.retry_failed, retry_uncertain=args.retry_uncertain)
    if command == "show":
        return runner.load_run(repo, args.id), 0
    if command == "history":
        if not 1 <= args.limit <= 1024:
            raise CoreError("history limit must be 1..1024")
        rows = read_jsonl(under(state_root(repo), "history.jsonl"))
        return {"schema": 1, "events": [row for row in rows if not args.id or row.get("id") == args.id][-args.limit:]}, 0
    action = args.action
    if command == "pack":
        if action == "inspect":
            return catalog.inspect_pack(str(_input_file(repo, args.directory))), 0
        if action == "import":
            return catalog.import_pack(repo, str(_input_file(repo, args.directory)), args.digest, args.resource), 0
        return {"schema": 1, "packs": catalog.list_packs(repo)}, 0
    if command == "workset":
        if action == "put":
            return catalog.put_workset(repo, load_definition(_input_file(repo, args.file)), replace=args.replace), 0
        if action == "show":
            return catalog.show_workset(repo, args.id), 0
        return {"schema": 1, "worksets": [catalog.show_workset(repo, row["id"]) for row in records(repo, "worksets")]}, 0
    if command == "trigger":
        if action == "add":
            return triggers.add(repo, load_definition(_input_file(repo, args.file))), 0
        if action == "list":
            return {"schema": 1, "triggers": records(repo, "triggers")}, 0
        if action in {"enable", "disable"}:
            return triggers.set_enabled(repo, args.id, action == "enable"), 0
        if action == "tick":
            return triggers.tick(repo, args.now or utc_now()), 0
        if action == "event":
            if len(args.data.encode("utf-8")) > MAX_DEFINITION_BYTES:
                raise CoreError("event data exceeds the definition bound")
            if args.data.lstrip().startswith("{"):
                payload = json.loads(args.data)
            else:
                payload = load_definition(_input_file(repo, args.data))
            return triggers.event(repo, args.name, args.event_id, payload), 0
        if action == "requests":
            return {"schema": 1, "requests": triggers.list_requests(repo)}, 0
        if action == "plan":
            return definitions.public_plan(triggers.request_plan(repo, args.id)), 0
        if action == "cancel":
            return triggers.cancel(repo, args.id), 0
        if action == "run":
            _execute_requested(args)
            return triggers.run_request(repo, args.id, retry_failed=args.retry_failed, retry_uncertain=args.retry_uncertain)
    raise CoreError("unsupported recipe action")


def main(argv=None) -> int:
    args = _parser().parse_args(argv)

    def interrupted(_signal, _frame):
        # selectors intentionally swallows InterruptedError as an interrupted
        # system call. A control-flow exception must reach the cleanup path.
        raise KeyboardInterrupt

    previous = signal.signal(signal.SIGTERM, interrupted)
    try:
        result, code = dispatch(args)
        print(json.dumps(result, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2))
        return code
    except CoreError as exc:
        print("error: " + str(exc), file=sys.stderr)
        return exc.exit_code
    except (OSError, ValueError, TypeError, KeyError, RecursionError):
        print("error: invalid or inaccessible recipe data", file=sys.stderr)
        return 2
    except (KeyboardInterrupt, InterruptedError):
        print("error: recipe invocation interrupted", file=sys.stderr)
        return 130
    finally:
        signal.signal(signal.SIGTERM, previous)
