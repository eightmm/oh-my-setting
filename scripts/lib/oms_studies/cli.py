"""Command-line contracts for local immutable study workflows."""
from __future__ import annotations

import argparse
import json
import signal
import sys
from pathlib import Path

from oms_runtime.common import CoreError, read_bytes, repo_root
from . import records as record_ops
from . import selection, suite, workflow
from .common import load, local, read_results, records, writable


def parser() -> argparse.ArgumentParser:
    def options(p, root=False):
        p.add_argument("--repo", default="." if root else argparse.SUPPRESS)
        p.add_argument("--pretty", action="store_true", default=False if root else argparse.SUPPRESS)
        return p

    def child(sub, name, help):
        return options(sub.add_parser(name, help=help))

    top = options(argparse.ArgumentParser(prog="oms study", description="Immutable local study workflows and explicit argv adapters"), True)
    commands = top.add_subparsers(dest="command", required=True)
    group = child(commands, "suite", "controlled benchmark suites and planned coverage")
    sub = group.add_subparsers(dest="action", required=True)
    p = child(sub, "create", "freeze fixtures, variant wrappers and one evaluator")
    p.add_argument("--spec", required=True); p.add_argument("--id")
    for action in ("show", "coverage", "run"):
        p = child(sub, action, action + " a registered suite")
        p.add_argument("--suite", required=True)
        if action == "run":
            p.add_argument("--missing", action="store_true")
            p.add_argument("--cell", action="append")
            p.add_argument("--limit", type=int)
            p.add_argument("--id")
    p = child(commands, "rescore", "evaluate existing hashed outputs without generation")
    p.add_argument("--results", required=True); p.add_argument("--evaluator", required=True)
    p.add_argument("--directions"); p.add_argument("--id")
    p = child(commands, "show", "read an immutable result batch")
    p.add_argument("--id", required=True)
    p = child(commands, "compare", "paired case/repeat differences between two variants")
    p.add_argument("--results", required=True); p.add_argument("--left", required=True); p.add_argument("--right", required=True)
    group = child(commands, "annotation", "append corrections and export selected cases")
    sub = group.add_subparsers(dest="action", required=True)
    p = child(sub, "add", "bind a correction to immutable source bytes")
    for flag in ("results", "run", "expected", "rationale"):
        p.add_argument("--" + flag, required=True)
    p.add_argument("--supersedes"); p.add_argument("--id")
    p = child(sub, "history", "read and verify the correction chain")
    p.add_argument("--run", required=True)
    p = child(sub, "export", "create a new suite from explicitly selected annotations")
    p.add_argument("--suite", required=True); p.add_argument("--annotation", required=True, action="append")
    p.add_argument("--id", required=True)
    group = child(commands, "baseline", "immutable named result bindings with rationale")
    sub = group.add_subparsers(dest="action", required=True)
    p = child(sub, "bind", "freeze an explicit completed-run selection")
    for flag in ("name", "results", "rationale"):
        p.add_argument("--" + flag, required=True)
    p.add_argument("--run", action="append")
    p = child(sub, "show", "verify a baseline and its artifact bytes")
    p.add_argument("--name", required=True)
    child(sub, "list", "list immutable baseline metadata")
    group = child(commands, "select", "pure Pareto selection or optional adaptive proposals")
    sub = group.add_subparsers(dest="action", required=True)
    p = child(sub, "pareto", "select a constrained nondominated front; no search")
    p.add_argument("--results", required=True); p.add_argument("--objectives"); p.add_argument("--constraints")
    p = child(sub, "propose", "optional Optuna TPE proposals; does not execute them")
    p.add_argument("--results", required=True); p.add_argument("--space", required=True)
    p.add_argument("--count", type=int, default=1); p.add_argument("--seed", type=int, default=0); p.add_argument("--id")
    group = child(commands, "workflow", "DVC JSON-encoded dvc.yaml only: cmd/deps/outs regular-file stages")
    group.description = "DVC adapter scope: one root JSON-encoded dvc.yaml, cmd/deps/outs regular-file stages, preserved stage names and outputs. No YAML parser, params/foreach/matrix/wdir, directory outputs, or built-in cache."
    sub = group.add_subparsers(dest="action", required=True)
    p = child(sub, "capture", "snapshot an already completed supported DVC pipeline")
    p.add_argument("--id", required=True); p.add_argument("--adapter", required=True); p.add_argument("--cache-dir")
    p = child(sub, "show", "read a workflow snapshot")
    p.add_argument("--id", required=True)
    for action in ("plan", "fork"):
        p = child(sub, action, action + " explicit replacement files over a snapshot")
        p.add_argument("--base", required=True); p.add_argument("--replacements", required=True)
        if action == "fork":
            p.add_argument("--id"); p.add_argument("--expected-plan-sha256")
    return top


def read_only(args) -> bool:
    if args.command in ("show", "compare"):
        return True
    allowed = {"suite": {"show", "coverage"}, "annotation": {"history"}, "baseline": {"show", "list"},
               "select": {"pareto"}, "workflow": {"show", "plan"}}
    return getattr(args, "action", None) in allowed.get(args.command, set())


def dispatch(args, repo: Path):
    def data(raw):
        return json.loads(read_bytes(local(repo, raw)).decode("utf-8"))

    if args.command == "suite":
        if args.action == "create":
            return suite.create(repo, data(args.spec), args.id)
        if args.action == "show":
            return load(repo, "suites", args.suite)
        if args.action == "coverage":
            return suite.coverage(repo, args.suite)
        return suite.run_suite(repo, args.suite, missing=args.missing, selected=args.cell, limit=args.limit, batch_id=args.id)
    if args.command == "rescore":
        return suite.rescore(repo, args.results, data(args.evaluator), pass_id=args.id,
                             metric_directions=data(args.directions) if args.directions else None)
    if args.command == "show":
        return load(repo, "results", args.id)
    if args.command == "compare":
        return selection.compare(read_results(repo, args.results)[0], args.left, args.right)
    if args.command == "annotation":
        if args.action == "add":
            return record_ops.annotate(repo, args.results, args.run, data(args.expected), args.rationale,
                                       supersedes=args.supersedes, annotation_id=args.id)
        if args.action == "history":
            return record_ops.history(repo, args.run)
        return record_ops.export_annotations(repo, args.suite, args.annotation, args.id)
    if args.command == "baseline":
        if args.action == "bind":
            return record_ops.bind_baseline(repo, args.name, args.results, args.rationale, args.run)
        if args.action == "show":
            return record_ops.baseline(repo, args.name)
        return {"schema": 1, "kind": "oms-study-baselines", "baselines": [
            {k: v.get(k) for k in ("name", "rationale", "created_at", "source_sha256", "content_sha256")}
            for v in records(repo, "baselines")]}
    if args.command == "select":
        if args.action == "pareto":
            return selection.pareto(read_results(repo, args.results)[0],
                                    data(args.objectives) if args.objectives else None,
                                    data(args.constraints) if args.constraints else None)
        return selection.propose(repo, args.results, data(args.space), count=args.count, seed=args.seed, proposal_id=args.id)
    if args.command == "workflow":
        if args.action == "capture":
            return workflow.capture(repo, args.id, data(args.adapter), cache_dir=args.cache_dir)
        if args.action == "show":
            return load(repo, "workflows", args.id)
        if args.action == "plan":
            return workflow.plan(repo, args.base, data(args.replacements))
        return workflow.fork(repo, args.base, data(args.replacements), name=args.id,
                             expected_plan_sha256=args.expected_plan_sha256)
    raise CoreError("unsupported study action")


def main(argv=None) -> int:
    args = parser().parse_args(argv)
    previous = None
    try:
        if not read_only(args):
            writable()
            if hasattr(signal, "SIGTERM"):
                previous = signal.getsignal(signal.SIGTERM)

                def interrupt(_signum, _frame):
                    raise KeyboardInterrupt

                signal.signal(signal.SIGTERM, interrupt)
        repo = repo_root(args.repo)
        value = dispatch(args, repo)
        print(json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2 if args.pretty else None))
        statuses = [r["status"] for r in value.get("rows", [])] if value.get("kind") == "oms-study-results" else [value.get("status")]
        if "interrupted" in statuses:
            return 130
        return 1 if "failed" in statuses else 0
    except CoreError as exc:
        print("study: %s" % exc, file=sys.stderr)
        return exc.exit_code
    except (OSError, ValueError, TypeError) as exc:
        print("study: %s" % exc, file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("study: interrupted; immutable attempt metadata remains available", file=sys.stderr)
        return 130
    finally:
        if previous is not None:
            signal.signal(signal.SIGTERM, previous)
