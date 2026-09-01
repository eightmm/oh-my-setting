"""Argparse front door for `oms graph` (W3 completes the dispatch)."""

from __future__ import annotations

import argparse
import json
import os
import sys
from typing import Any, Optional, Sequence

from . import GRAPH_PACKAGE_VERSION
from .child_policy import CHILD_GRAPH_ERROR, child_action_is_read_only
from .errors import GraphError
from oms_runtime.common import CoreError


def emit(value: Any, pretty: bool = False) -> None:
    print(json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2 if pretty else None, separators=None if pretty else (",", ":")))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="oms graph", description="Project graph and execution graph over the existing OMS control plane.")
    parser.add_argument("--repo", default=".", help="project repository or directory")
    parser.add_argument("--pretty", action="store_true", help="pretty-print JSON")
    parser.add_argument("--version", action="version", version=GRAPH_PACKAGE_VERSION)
    groups = parser.add_subparsers(dest="group", required=True)
    project = groups.add_parser("project", help="structural code graph: build, query, blast radius, context packs")
    project_sub = project.add_subparsers(dest="action", required=True)
    build = project_sub.add_parser("build")
    build.add_argument("--force", action="store_true")
    build.add_argument("--include", action="append", default=[])
    build.add_argument("--exclude", action="append", default=[])
    build.add_argument("--max-bytes", type=int, default=2 * 1024 * 1024)
    project_sub.add_parser("check")
    show_map = project_sub.add_parser("map")
    show_map.add_argument("--json", action="store_true")
    show_map.add_argument("--mermaid", action="store_true")
    find = project_sub.add_parser("find")
    find.add_argument("query")
    find.add_argument("--kind", default="")
    find.add_argument("--limit", type=int, default=20)
    find.add_argument("--json", action="store_true")
    neighbors = project_sub.add_parser("neighbors")
    neighbors.add_argument("node")
    neighbors.add_argument("--relation", default="")
    neighbors.add_argument("--direction", default="both", choices=("in", "out", "both"))
    neighbors.add_argument("--json", action="store_true")
    trace = project_sub.add_parser("trace")
    trace.add_argument("node")
    trace.add_argument("--direction", default="out", choices=("in", "out"))
    trace.add_argument("--depth", type=int, default=2)
    trace.add_argument("--json", action="store_true")
    blast = project_sub.add_parser("blast")
    blast.add_argument("--base", default="")
    blast.add_argument("--path", action="append", default=[])
    blast.add_argument("--depth", type=int, default=3)
    blast.add_argument("--json", action="store_true")
    analyze = project_sub.add_parser("analyze")
    analyze.add_argument("--hubs", type=int, default=10)
    analyze.add_argument("--cycles", action="store_true")
    analyze.add_argument("--communities", action="store_true")
    analyze.add_argument("--path", nargs=2, default=None, metavar=("FROM", "TO"))
    analyze.add_argument("--json", action="store_true")
    context = project_sub.add_parser("context")
    context.add_argument("--task", required=True)
    context.add_argument("--max-files", type=int, default=12)
    context.add_argument("--bundle", action="store_true")
    context.add_argument("--base", default="")
    context.add_argument("--json", action="store_true")
    execution = groups.add_parser("exec", help="execution graph: validate, route, run, resume, decide, status, events")
    exec_sub = execution.add_subparsers(dest="action", required=True)
    validate = exec_sub.add_parser("validate")
    validate.add_argument("spec")
    validate.add_argument("--json", action="store_true")
    render = exec_sub.add_parser("render")
    render.add_argument("spec")
    render.add_argument("--mermaid", action="store_true")
    route = exec_sub.add_parser("route")
    route.add_argument("spec", nargs="?", default="")
    route.add_argument("--run", default="")
    route.add_argument("--facts", default="")
    route.add_argument("--outcomes", default="")
    route.add_argument("--json", action="store_true")
    run = exec_sub.add_parser("run")
    run.add_argument("spec")
    run.add_argument("--worker", required=True)
    run.add_argument("--model", default="")
    run.add_argument("--reasoning-effort", default="")
    run.add_argument("--max-steps", type=int, default=None)
    run.add_argument("--jobs", type=int, default=1)
    run.add_argument("--goal", default="")
    run.add_argument("--run", default="")
    run.add_argument("--dry-run", action="store_true")
    resume = exec_sub.add_parser("resume")
    resume.add_argument("--run", required=True)
    resume.add_argument("--worker", required=True)
    resume.add_argument("--model", default="")
    resume.add_argument("--reasoning-effort", default="")
    decide = exec_sub.add_parser("decide")
    decide.add_argument("--run", required=True)
    decide.add_argument("--node", required=True)
    decide.add_argument("--outcome", required=True)
    decide.add_argument("--note", default="")
    status = exec_sub.add_parser("status")
    status.add_argument("--run", default="")
    status.add_argument("--json", action="store_true")
    status.add_argument("--mermaid", action="store_true")
    events = exec_sub.add_parser("events")
    events.add_argument("--run", required=True)
    events.add_argument("--limit", type=int, default=50)
    events.add_argument("--json", action="store_true")
    shadow = exec_sub.add_parser("shadow")
    shadow.add_argument("--spec", default="goal-drive")
    shadow.add_argument("--json", action="store_true")
    test = exec_sub.add_parser("test")
    test.add_argument("path")
    test.add_argument("--json", action="store_true")
    return parser


def dispatch(args: argparse.Namespace) -> int:
    raise GraphError("oms graph %s %s is not implemented yet" % (args.group, args.action))


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if os.environ.get("OMS_HARNESS_CHILD") == "1" and not child_action_is_read_only(args):
            raise GraphError(CHILD_GRAPH_ERROR)
        return dispatch(args)
    except CoreError as exc:
        print("graph: %s" % exc, file=sys.stderr)
        return exc.exit_code
    except KeyboardInterrupt:
        print("graph: interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
