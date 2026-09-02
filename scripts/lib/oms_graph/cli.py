"""Argparse front door for `oms graph`."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

from . import GRAPH_PACKAGE_VERSION
from . import commit as exec_commit
from . import events as exec_events
from . import render
from . import route as exec_route
from . import runner
from . import shadow as exec_shadow
from .child_policy import CHILD_GRAPH_ERROR, child_action_is_allowed
from .errors import GraphError
from .facts import collect_facts
from .project import analytics
from .project import blast as project_blast
from .project import history as project_history
from .project import build as project_build
from .project import context as project_context
from .project.query import Graph
from .spec import load_spec
from .validate import validate_spec
from oms_runtime.common import CoreError, read_json, repo_root

PROJECT_STATE_ENV = "OMS_PROJECT_GRAPH_STATE"
AUTOBUILD_ENV = "OMS_GRAPH_AUTOBUILD"
# Readers that keep the graph current themselves, so `oms graph project find`
# works in a repository nobody has built yet. `check` reports freshness and
# `build` is the explicit form: neither may refresh behind the caller's back.
AUTO_REFRESH_ACTIONS = ("map", "find", "neighbors", "trace", "blast", "analyze", "coupling", "context")


def emit(value: Any, pretty: bool = False) -> None:
    print(json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2 if pretty else None, separators=None if pretty else (",", ":")))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="oms graph", description="Project graph and execution graph over the existing OMS control plane.")
    parser.add_argument("--repo", default=".", help="project repository or directory")
    parser.add_argument("--pretty", action="store_true", help="pretty-print JSON")
    parser.add_argument("--version", action="version", version=GRAPH_PACKAGE_VERSION)
    groups = parser.add_subparsers(dest="group", required=True)
    project = groups.add_parser("project", help="structural code graph: build, query, blast radius, context packs",
                               description="Structural code graph over --repo. State lives in <repo>/.oms/project-graph/ unless %s names an absolute directory to use instead. Readers build or refresh it themselves unless --no-refresh or %s=0 says otherwise." % (PROJECT_STATE_ENV, AUTOBUILD_ENV))
    project_sub = project.add_subparsers(dest="action", required=True)
    build = project_sub.add_parser("build")
    build.add_argument("--force", action="store_true")
    build.add_argument("--include", action="append", default=[])
    build.add_argument("--exclude", action="append", default=[])
    build.add_argument("--max-bytes", type=int, default=2 * 1024 * 1024)
    build.add_argument("--json", action="store_true")
    ensure = project_sub.add_parser("ensure")
    ensure.add_argument("--max-files", type=int, default=project_build.AUTOBUILD_MAX_FILES)
    ensure.add_argument("--json", action="store_true")
    check = project_sub.add_parser("check")
    check.add_argument("--json", action="store_true")
    show_map = project_sub.add_parser("map")
    show_map.add_argument("--include-tests", action="store_true", help="keep test files in the overview (dropped by default)")
    show_map.add_argument("--json", action="store_true")
    show_map.add_argument("--mermaid", action="store_true")
    find = project_sub.add_parser("find")
    find.add_argument("query")
    find.add_argument("--kind", default="")
    find.add_argument("--limit", type=int, default=20)
    find.add_argument("--include-tests", action="store_true", help="rank test files too (dropped by default unless --kind test)")
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
    blast.add_argument("--limit", type=int, default=0, metavar="N",
                       help="keep at most N dependents, files, tests per list in JSON (0 = all; the text view is never cut)")
    blast.add_argument("--json", action="store_true")
    coupling = project_sub.add_parser("coupling", help="paths that change together in Git history; structural=no marks coupling the parsers cannot see")
    coupling.add_argument("--path", action="append", default=[], help="keep pairs touching this path (repeatable)")
    coupling.add_argument("--commits", type=int, default=project_history.DEFAULT_COMMITS, help="history window, newest first")
    coupling.add_argument("--max-changeset", type=int, default=project_history.DEFAULT_MAX_CHANGESET, help="skip commits touching more files than this")
    coupling.add_argument("--min-shared", type=int, default=project_history.DEFAULT_MIN_SHARED, help="minimum commits a pair shared")
    coupling.add_argument("--min-degree", type=float, default=project_history.DEFAULT_MIN_DEGREE, help="minimum coupling degree, percent of average revisions")
    coupling.add_argument("--limit", type=int, default=40)
    coupling.add_argument("--json", action="store_true")
    analyze = project_sub.add_parser("analyze")
    analyze.add_argument("--hubs", type=int, default=10)
    analyze.add_argument("--cycles", action="store_true")
    analyze.add_argument("--communities", action="store_true")
    analyze.add_argument("--path", nargs=2, default=None, metavar=("FROM", "TO"))
    analyze.add_argument("--include-tests", action="store_true", help="keep test files in hubs, cycles, and communities")
    analyze.add_argument("--json", action="store_true")
    context = project_sub.add_parser("context")
    context.add_argument("--task", required=True)
    context.add_argument("--max-files", type=int, default=12)
    context.add_argument("--bundle", action="store_true")
    context.add_argument("--base", default="")
    context.add_argument("--json", action="store_true")
    for reader in (show_map, find, neighbors, trace, blast, analyze, coupling, context):
        reader.add_argument("--no-refresh", action="store_true", help="read the graph as it stands; never build or refresh it")
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
    run.add_argument("--json", action="store_true")
    resume = exec_sub.add_parser("resume")
    resume.add_argument("--run", required=True)
    resume.add_argument("--worker", required=True)
    resume.add_argument("--model", default="")
    resume.add_argument("--reasoning-effort", default="")
    resume.add_argument("--max-steps", type=int, default=None)
    resume.add_argument("--jobs", type=int, default=1)
    resume.add_argument("--goal", default="")
    resume.add_argument("--json", action="store_true")
    decide = exec_sub.add_parser("decide")
    decide.add_argument("--run", required=True)
    decide.add_argument("--node", required=True)
    decide.add_argument("--outcome", required=True)
    decide.add_argument("--note", default="")
    decide.add_argument("--json", action="store_true")
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
    commit = exec_sub.add_parser("commit", help="commit exactly the landed patch of a bound task (parent-only)")
    commit.add_argument("--binding", required=True)
    commit.add_argument("--run", default=os.environ.get("OMS_GRAPH_RUN_ID", ""))
    commit.add_argument("--message", default="")
    commit.add_argument("--json", action="store_true")
    return parser


def _project_state(args: argparse.Namespace) -> Tuple[Path, Path]:
    """Repository root plus the project-graph state directory this run writes to."""
    repo = repo_root(args.repo)
    override = os.environ.get(PROJECT_STATE_ENV, "").strip()
    if override and not os.path.isabs(override):
        raise GraphError("%s must be an absolute path: %s" % (PROJECT_STATE_ENV, override))
    return repo, project_build.state_dir(repo, Path(override) if override else None)


def _index(repo: Path, state: Path) -> Tuple[Dict[str, Any], Graph]:
    graph = project_build.load_graph(repo, state=state)
    return graph, Graph(graph)


def _build_summary(action: str, revision: str, stats: Dict[str, int], skipped: int) -> str:
    return ("graph: %s revision=%s files=%d nodes=%d edges=%d cached=%d parsed=%d skipped=%d"
            % (action, revision[:12], stats["files"], stats["nodes"], stats["edges"], stats["cached"], stats["parsed"], skipped))


def _ensure_summary(result: Dict[str, Any]) -> str:
    if result["action"] == "fresh":
        return "graph: fresh revision=%s" % result["revision"][:12]
    return _build_summary(result["action"], result["revision"], result["stats"], result["skipped"])


def _project_build(args: argparse.Namespace, repo: Path, state: Path) -> int:
    manifest = project_build.build(repo, state=state, include=args.include, exclude=args.exclude, max_bytes=args.max_bytes, force=args.force)
    if args.json:
        emit(manifest, args.pretty)
        return 0
    print(_build_summary("built", manifest["revision"], manifest["stats"], len(manifest["skipped"])))
    return 0


def _project_ensure(args: argparse.Namespace, repo: Path, state: Path) -> int:
    result = project_build.ensure(repo, state=state, max_files=args.max_files)
    if args.json:
        emit(result, args.pretty)
        return 0
    print(_ensure_summary(result))
    return 0


def _project_check(args: argparse.Namespace, repo: Path, state: Path) -> int:
    result = project_build.check(repo, state=state)
    if args.json:
        emit(result, args.pretty)
        return 0 if result["fresh"] else 3
    if result["fresh"]:
        print("fresh")
        return 0
    if not result["present"]:
        print("absent: run: oms graph project build")
    for label in ("stale", "missing", "new"):
        for path in result[label]:
            print("%s: %s" % (label, path))
    return 3


def _project_map(args: argparse.Namespace, repo: Path, state: Path) -> int:
    graph, index = _index(repo, state)
    if not args.include_tests:
        index = index.without_tests()
        graph = index.graph
    summary = index.map_summary()
    if args.json:
        emit(summary, args.pretty)
    elif args.mermaid:
        print(render.render_project_mermaid(graph.get("nodes", []), graph.get("edges", []), limit=200))
    else:
        print(render.render_project_map_text(summary))
    return 0


def _project_find(args: argparse.Namespace, repo: Path, state: Path) -> int:
    rows = _index(repo, state)[1].find(args.query, kinds=(args.kind,) if args.kind else (), limit=args.limit, include_tests=args.include_tests)
    if args.json:
        emit(rows, args.pretty)
        return 0
    for row in rows:
        print("%s  %s  %s  %d" % (row["id"], row["kind"], row.get("path", ""), row["score"]))
    return 0


def _project_neighbors(args: argparse.Namespace, repo: Path, state: Path) -> int:
    rows = _index(repo, state)[1].neighbors(args.node, relation=args.relation, direction=args.direction)
    if args.json:
        emit(rows, args.pretty)
        return 0
    for row in rows:
        print("%s  %s  %s  %s" % (row["direction"], row["relation"], row["confidence"], row["id"]))
    return 0


def _project_trace(args: argparse.Namespace, repo: Path, state: Path) -> int:
    result = _index(repo, state)[1].trace(args.node, direction=args.direction, depth=args.depth)
    if args.json:
        emit(result, args.pretty)
        return 0
    for row in result["nodes"]:
        via = "  via %s" % row["via"] if row["via"] else ""
        print("%s%s%s" % ("  " * int(row["distance"]), row["id"], via))
    return 0


def _project_blast(args: argparse.Namespace, repo: Path, state: Path) -> int:
    changed = project_blast.changed_paths(repo, base=args.base)
    paths = sorted(set(args.path)) if args.path else sorted(set(changed["changed"]) | set(changed["untracked"]))
    result = project_blast.blast_radius(_index(repo, state)[1], paths, depth=args.depth)
    if args.json:
        payload = dict(result)
        payload["paths"] = paths
        payload["changed_paths"] = changed
        if args.limit and args.limit > 0:
            # A projection, not the closure: every list is cut to the same bound
            # and the omitted counts say so, so a bounded consumer (MCP) never
            # receives a payload truncated mid-object.
            omitted = {}
            for key in ("dependents", "files", "tests", "test_cases", "seeds"):
                rows = payload.get(key)
                if isinstance(rows, list) and len(rows) > args.limit:
                    omitted[key] = len(rows) - args.limit
                    payload[key] = rows[:args.limit]
            payload["limits"] = {"per_list": args.limit}
            payload["truncated"] = bool(omitted)
            payload["omitted"] = omitted
        emit(payload, args.pretty)
        return 0
    for path in paths:
        print("path: %s" % path)
    for ident in result["seeds"]:
        print("seed: %s" % ident)
    for row in result["dependents"]:
        print("%d  %s  (%s)" % (row["distance"], row["id"], row["via"]))
    print("files:")
    for path in result["files"]:
        print("  %s" % path)
    print("tests:")
    for path in result["tests"]:
        print("  %s" % path)
    return 0


def _project_context(args: argparse.Namespace, repo: Path, state: Path) -> int:
    pack = project_context.context_pack(repo, _index(repo, state)[1], task=args.task, max_files=args.max_files, base=args.base, state=state)
    if args.bundle:
        pack["bundle"] = project_context.compile_bundle(repo, pack)
    if args.json:
        emit(pack, args.pretty)
        return 0
    print("task: %s" % pack["task"])
    for entry in pack["entries"]:
        print("entry: %s" % entry)
    print("files:")
    for item in pack["evidence"]:
        print("  %s  %s  %d" % (item["path"], item["reason"], item["score"]))
    print("tests:")
    for path in pack["tests"]:
        print("  %s" % path)
    estimate = pack["byte_estimate"]
    print("byte_estimate: raw_candidate_files=%d pack=%d" % (estimate["raw_candidate_files"], estimate["pack"]))
    print("pack_path: %s" % pack["pack_path"])
    if args.bundle:
        print("bundle: selected_bytes=%d path=%s" % (pack["bundle"]["selected_bytes"], pack["bundle"]["bundle_path"]))
    return 0


def _project_coupling(args: argparse.Namespace, repo: Path, state: Path) -> int:
    if args.commits < 1 or args.max_changeset < 2 or args.min_shared < 1 or args.min_degree < 0 or args.limit < 0:
        raise GraphError("coupling: --commits >= 1, --max-changeset >= 2, --min-shared >= 1, --min-degree >= 0, --limit >= 0")
    graph, _ = _index(repo, state)
    report = project_history.coupling_report(repo, graph, commits=args.commits, max_changeset=args.max_changeset,
                                             min_shared=args.min_shared, min_degree=args.min_degree,
                                             focus=args.path, limit=args.limit)
    if args.json:
        emit(report, args.pretty)
        return 0
    print("co-change coupling: %d commits read (%d bulk commits skipped), min shared %d, min degree %.0f%%, %d pair(s) shown%s, %d without a structural edge"
          % (report["commits"], report["skipped_bulk"], args.min_shared, args.min_degree, len(report["pairs"]),
             " of %d" % (len(report["pairs"]) + report["omitted"]) if report["truncated"] else "", report["hidden"]))
    for row in report["pairs"]:
        print("%5.1f%%  shared %2d  revs %d/%d  structural=%s  %s  <->  %s"
              % (row["degree"], row["shared_revs"], row["revs"][0], row["revs"][1], "yes" if row["structural"] else "no", row["a"], row["b"]))
    return 0


def _project_analyze(args: argparse.Namespace, repo: Path, state: Path) -> int:
    graph, index = _index(repo, state)
    if not args.include_tests:
        graph = index.without_tests().graph
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])
    result: Dict[str, Any] = {
        "schema": 1,
        "hubs": analytics.hubs(nodes, edges, limit=args.hubs),
        "components": [{"size": len(members), "members": members} for members in analytics.connected_components(nodes, edges)],
    }
    if args.cycles:
        result["cycles"] = analytics.cycles(nodes, edges)
    if args.communities:
        result["communities"] = analytics.communities(nodes, edges)
    if args.path:
        result["path"] = analytics.shortest_path(nodes, edges, args.path[0], args.path[1])
    if args.json:
        emit(result, args.pretty)
        return 0
    print("hubs:")
    for position, row in enumerate(result["hubs"], 1):
        print("  %2d. %s  (%s)  degree %d" % (position, row["id"], row["kind"], row["degree"]))
    sizes = [row["size"] for row in result["components"]]
    print("components: %d (largest %d)" % (len(sizes), sizes[0] if sizes else 0))
    for cycle in result.get("cycles", []):
        print("cycle: %s" % " -> ".join(cycle))
    for community in result.get("communities", []):
        print("community %s (%s): %d nodes" % (community["id"], community["label"], len(community["members"])))
    if args.path:
        path = result.get("path")
        print("path: %s" % (" -> ".join(path) if path else "none"))
    return 0


PROJECT_ACTIONS = {"build": _project_build, "ensure": _project_ensure, "check": _project_check, "map": _project_map,
                   "find": _project_find, "neighbors": _project_neighbors, "trace": _project_trace,
                   "blast": _project_blast, "analyze": _project_analyze, "coupling": _project_coupling,
                   "context": _project_context}

STOP_STATUSES = ("gate", "blocked", "exhausted", "waiting", "invalid")


def _json_option(value: str) -> Dict[str, Any]:
    """A JSON object given inline or as `@file`; an empty option means none."""
    text = str(value or "").strip()
    if not text:
        return {}
    parsed = read_json(Path(text[1:]), default=None) if text.startswith("@") else None
    if parsed is None and not text.startswith("@"):
        try:
            parsed = json.loads(text)
        except ValueError as exc:
            raise GraphError("option is not a JSON object: %s" % exc)
    if not isinstance(parsed, dict):
        raise GraphError("option must be a JSON object")
    return parsed


def _run_view(repo: Path, run_id: str) -> Tuple[Dict[str, Any], Dict[str, Any], Dict[str, Any]]:
    """Frozen spec, events projection, and current facts for one recorded run."""
    spec = exec_events.load_run_spec(repo, run_id)
    rows = exec_events.read_events(repo, run_id)
    projection = exec_events.project(rows, spec)
    return spec, projection, runner.alias_facts(collect_facts(repo), rows, spec)


def _exec_validate(args: argparse.Namespace) -> int:
    verdict = validate_spec(load_spec(args.spec))
    if args.json:
        emit(verdict, args.pretty)
        return 0 if verdict["ok"] else 3
    if verdict["ok"]:
        print("ok")
    for label, items in (("error", verdict["errors"]), ("warning", verdict["warnings"])):
        for item in items:
            print("%s %s %s: %s" % (label, item["code"], item["where"], item["message"]))
    return 0 if verdict["ok"] else 3


def _exec_render(args: argparse.Namespace) -> int:
    spec = load_spec(args.spec)
    sys.stdout.write(render.render_exec_mermaid(spec) if args.mermaid else render.render_exec_text(spec))
    return 0


def _print_route(route: Dict[str, Any]) -> None:
    print("%s %s %s" % (route["status"], route["primary"] or "-", route["reason"]))
    for item in route["alternatives"]:
        print("alternative: %s" % item)
    for item in route["downgrades"]:
        print("downgrade: %s claimed=%s effective=%s missing=%s"
              % (item.get("node"), item.get("claimed"), item.get("effective"), ",".join(item.get("missing", []))))


def _exec_route(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    if args.run:
        spec, state, facts = _run_view(repo, args.run)
    elif args.spec:
        spec = load_spec(args.spec)
        state = exec_route.state_from_outcomes(spec, _json_option(args.outcomes))
        facts = read_json(Path(args.facts), default=None) if args.facts else collect_facts(repo)
        if not isinstance(facts, dict):
            raise GraphError("fact file must hold a JSON object: %s" % args.facts)
    else:
        raise GraphError("exec route needs a spec or --run")
    route = exec_route.evaluate(spec, state, facts)
    if args.json:
        emit(route, args.pretty)
        return 0
    _print_route(route)
    return 0


def _exec_run(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    result = runner.run(repo, args.spec, worker=args.worker, run_id=args.run, model=args.model,
                        reasoning_effort=args.reasoning_effort, max_steps=args.max_steps,
                        jobs=args.jobs, goal=args.goal, dry_run=args.dry_run)
    return _report_run(args, result)


def _report_run(args: argparse.Namespace, result: Dict[str, Any]) -> int:
    if result.get("dry_run"):
        if args.json:
            emit(result, args.pretty)
        else:
            print("run: dry-run status=%s primary=%s" % (result["route"]["status"], result["route"]["primary"] or "-"))
            for argv in result["commands"]:
                print("command: %s" % " ".join(argv))
        return 0
    if args.json:
        emit(result, args.pretty)
    else:
        print("run: %s status=%s primary=%s" % (result["run_id"], result["status"], result["primary"] or "-"))
        if result.get("reason"):
            print("reason: %s" % result["reason"])
    return 3 if result["status"] in STOP_STATUSES else 0


def _exec_resume(args: argparse.Namespace) -> int:
    result = runner.resume(repo_root(args.repo), args.run, worker=args.worker, model=args.model,
                           reasoning_effort=args.reasoning_effort, max_steps=args.max_steps,
                           jobs=args.jobs, goal=args.goal)
    return _report_run(args, result)


def _exec_decide(args: argparse.Namespace) -> int:
    result = runner.decide(repo_root(args.repo), args.run, args.node, args.outcome, note=args.note)
    if args.json:
        emit(result, args.pretty)
        return 0
    print("decide: %s %s=%s" % (args.run, args.node, args.outcome))
    _print_route(result["route"])
    return 0


def _exec_status(args: argparse.Namespace) -> int:
    repo = repo_root(args.repo)
    run_id = args.run or exec_events.latest_run_id(repo)
    if not run_id:
        print("graph: no execution graph run is recorded; run: oms graph exec run SPEC --worker P", file=sys.stderr)
        return 3
    spec, projection, facts = _run_view(repo, run_id)
    route = exec_route.evaluate(spec, projection, facts)
    if args.json:
        emit({"schema": 1, "run_id": run_id, "spec_id": spec.get("id", ""), "route": route, "projection": projection}, args.pretty)
        return 0
    if args.mermaid:
        sys.stdout.write(render.render_exec_mermaid(spec, projection))
        return 0
    print("run: %s spec=%s status=%s" % (run_id, spec.get("id", ""), route["status"]))
    bindings = projection.get("bindings", {})
    if bindings:
        print("bindings:")
        for name in sorted(bindings):
            entry = bindings[name]
            print("  %s -> %s (bound by %s#%s)" % (name, entry.get("task_id", "-"), entry.get("node", "-"), entry.get("attempt", "-")))
    sys.stdout.write(render.render_exec_text(spec, projection, route))
    return 0


def _exec_events(args: argparse.Namespace) -> int:
    rows = exec_events.read_events(repo_root(args.repo), args.run)
    selected = rows[-args.limit:] if args.limit > 0 else rows
    if args.json:
        emit({"schema": 1, "run_id": args.run, "events": selected}, args.pretty)
        return 0
    for row in selected:
        print("%s %s %s %s %s" % (row.get("seq"), row.get("ts"), row.get("event"),
                                  row.get("node") or "-", row.get("outcome") or row.get("status") or "-"))
    return 0


def _exec_shadow(args: argparse.Namespace) -> int:
    row = exec_shadow.shadow(repo_root(args.repo), spec_name=args.spec)
    if args.json:
        emit(row, args.pretty)
        return 0
    print("shadow: agree=%s basis=%s route=%s/%s settled=%s control=%s->%s"
          % (row["agree"], row.get("basis") or "-", row["route"]["status"], row["route"]["primary"] or "-",
             ",".join(row.get("reconstructed", {}).get("completed", [])) or "-",
             row["control_plane"]["action"] or "-", row["control_plane"]["mapped"] or "-"))
    return 0


def _exec_test(args: argparse.Namespace) -> int:
    path = Path(args.path)
    files = sorted(path.glob("*.json")) if path.is_dir() else [path]
    if not files:
        raise GraphError("no route fixtures found: %s" % args.path)
    rows: List[Dict[str, Any]] = []
    for item in files:
        fixture = read_json(item, default=None)
        if not isinstance(fixture, dict):
            raise GraphError("route fixture must be a JSON object: %s" % item.name)
        ok, detail = exec_route.run_fixture(fixture)
        rows.append({"name": str(fixture.get("name") or item.stem), "ok": ok, "diff": detail["diff"]})
    passed = all(row["ok"] for row in rows)
    if args.json:
        emit({"schema": 1, "ok": passed, "results": rows}, args.pretty)
        return 0 if passed else 3
    for row in rows:
        print("PASS %s" % row["name"] if row["ok"] else "FAIL %s: %s" % (row["name"], json.dumps(row["diff"], sort_keys=True)))
    return 0 if passed else 3


def _exec_commit(args: argparse.Namespace) -> int:
    result = exec_commit.commit_bound(repo_root(args.repo), binding=args.binding, run_id=args.run, message=args.message)
    if args.json:
        emit(result, args.pretty)
        return 0
    print("commit: %s task=%s status=%s paths=%d" % (result["commit"][:12], result["task_id"], result["status"], len(result["paths"])))
    return 0


EXEC_ACTIONS = {"validate": _exec_validate, "render": _exec_render, "route": _exec_route, "run": _exec_run,
                "resume": _exec_resume, "decide": _exec_decide, "status": _exec_status, "events": _exec_events,
                "shadow": _exec_shadow, "test": _exec_test, "commit": _exec_commit}


def _auto_refresh(args: argparse.Namespace, repo: Path, state: Path) -> None:
    """Keep a reader's graph current, so no verb ever asks its caller to go build one.

    The summary goes to stderr: a build is a side effect of the read, and
    `--json` stdout has to stay parsable.
    """
    if args.action not in AUTO_REFRESH_ACTIONS or getattr(args, "no_refresh", False):
        return
    if os.environ.get(AUTOBUILD_ENV, "").strip() == "0":
        return
    result = project_build.ensure(repo, state=state)
    if result["action"] != "fresh":
        print(_ensure_summary(result), file=sys.stderr)


def dispatch(args: argparse.Namespace) -> int:
    if args.group == "project" and args.action in PROJECT_ACTIONS:
        repo, state = _project_state(args)
        _auto_refresh(args, repo, state)
        return PROJECT_ACTIONS[args.action](args, repo, state)
    if args.group == "exec" and args.action in EXEC_ACTIONS:
        return EXEC_ACTIONS[args.action](args)
    raise GraphError("oms graph %s %s is not a known action" % (args.group, args.action))


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if os.environ.get("OMS_HARNESS_CHILD") == "1" and not child_action_is_allowed(args):
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
