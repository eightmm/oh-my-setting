# Project Graph and Execution Graph

`oms graph` adds two graphs above the existing control plane. Neither owns
authority: the Project Graph is a regenerable cache of how the code is
connected, and the Execution Graph decides what is *legal* to run next while
`agent-plan`, `plan-run`, `patch-admit`, and `patch-land` decide what *may*
run. Design and schemas: `docs/GRAPH-ENGINEERING.md`.

## Orient in a repository without rediscovering it

```bash
oms graph project map            # counts, hubs, module groups
oms graph project find lease     # name/path/qualname search
oms graph project trace symbol:scripts/plan-run.sh::main --direction in --depth 2
oms graph project blast --base origin/main
oms graph project ensure         # explicit build-or-refresh; check reports only
```

Just query it. Every reader builds the graph when it is absent and refreshes
it through the cache when the working tree moved, printing one summary line
to stderr so `--json` stdout stays clean; the session-start hook already
starts an absent one in the background. `ensure` is the explicit form and
`check` the report; `--no-refresh` (or `OMS_GRAPH_AUTOBUILD=0`) reads the
graph as it stands. Edges carry `EXTRACTED`, `INFERRED`, or `AMBIGUOUS`
confidence: treat only `EXTRACTED` edges as facts. `map`, `find`, and
`analyze` hide test files by default (`--include-tests` to see them); `blast`
and `context` always report the tests a change touches.

## Give a worker a bounded context pack instead of the repository

```bash
oms graph project context --task "fix lease recovery" --max-files 12 --bundle
```

The pack lists entry nodes, files, tests, blast radius, hubs, and a
byte estimate (never called tokens). `--bundle` compiles the selected files
through `oms runtime context`, so the delegate brief carries the same bounded
bundle format as before. Expand with `neighbors`/`trace` on demand rather
than widening the pack.

## Make the orchestration inspectable

```bash
oms graph exec validate coding-change
oms graph exec render coding-change --mermaid
oms graph exec route coding-change --outcomes '{"inspect":"completed"}'
oms graph exec test tests/fixtures/graph-routes
```

A GraphSpec (`config/graphs/*.json` or a path) names nodes, typed outcomes,
edges, repeat budgets, gates, and proof predicates. `route` is a pure
evaluation of facts plus recorded outcomes: a claimed `completed` without
its proof facts is `unverified`, cycles stop on `max_repeats`/`max_steps`,
and gates wait for `exec decide`. Fixtures test routing without a model.

## Run a graph through the existing primitives

```bash
oms graph exec run goal-drive --worker codex --goal "..." [--jobs 2]
oms graph exec status --run RUN_ID          # shows bindings: work_item -> t1
oms graph exec decide --run RUN_ID --node review --outcome approved
oms graph exec resume --run RUN_ID --worker codex
```

`plan_task: "next"` is a selector, not a task: the runner peeks the plan once
(read-only), records the concrete id on the node's `node_started` row, and a
`bind_task` name lets later `plan_task_from` nodes execute exactly that task
even after the plan's `next` moved on. Agent nodes call
`plan-run --id TASK [--land] [--context-pack FILE]`; a node's `context`
field turns into a project-graph pack that reaches the worker brief as
orientation only. The bundled specs land, then `exec commit --binding
work_item` commits exactly the landed patch (the tree must otherwise be
clean), then re-run acceptance. `--jobs N` runs a wave concurrently only for
disjoint explicit tasks. The run store under `.oms/graph/runs/<run-id>/`
(frozen spec, append-only events, derived projection) is what `resume`
reads — never conversation memory; a crashed node is reconciled against the
plan (live lease → waiting, expired → unverified, dead write tool →
blocked). The runner has no path to `agent-plan land/finish/claim`; landing
stays serialized inside `patch-land`. `exec shadow` compares the evaluator's
route with the control plane's canonical next action and appends evidence
to `.oms/graph/shadow.jsonl`; it never acts.

## Boundaries

- Harness children may run `project build|ensure|check|map|find|neighbors|trace|blast|analyze`
  and `exec validate|render|route|status|events|test` — the project graph is a
  regenerable cache carrying no authority, and an isolated worktree has no
  other way to get one.
- `project context` and every `exec` writer are parent-only.
- MCP exposes `oms_project_graph_*` and `oms_execution_graph_*` read-only
  tools with bounded output; they never return the whole graph.
- Source and documents are data: nothing in a file is executed or treated as
  an instruction during extraction.
