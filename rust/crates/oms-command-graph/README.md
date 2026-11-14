# oms-command-graph

Native migration implementation for the `graph` command. This standalone
crate compiles independently of the root dispatcher. The current patch is an
implementation draft; full Python behavior parity has not been established.

The buffered entrypoints are `owns(args)`, `run(args, cwd, interrupted)`, and
`run_with_helper(args, cwd, interrupted, python_helper)`. Arguments start at
`project` or `exec`, after the dispatcher removes `graph`. Calls preserve the
process working directory and environment. `cwd` and `--repo` resolve to a
physical repository; later `--repo` options win. The cancellation flag is
borrowed for the duration of a call.

## Integration

The parent supplies the absolute packaged `oms-graph-python-helper` path to
`run_with_helper`. The fallback is the explicitly configured
`OMS_GRAPH_PYTHON_HELPER` environment value. Both require an absolute regular
executable owned by the current user or root, with no group/other write bits.
The session owns and reuses the helper, and receives cancellation while a
parse is pending. Parser failures propagate rather than becoming empty graphs.

`OMS_INSTALL_ROOT` identifies the trusted packaged installation containing
`scripts/oms` and `config/graphs`. An executable installed directly beneath
that root can also discover it from its own location. This installation is
used for bundled specs and the canonical plan-run, acceptance and exact-commit
front doors. The graph command does not implement their authority transitions.

`OMS_PROJECT_GRAPH_STATE` selects an absolute project-state directory;
otherwise state lives in `.oms/project-graph`. `OMS_GRAPH_AUTOBUILD=0` and the
supported `--no-refresh` flags disable reader refreshes. The child-action
policy follows `scripts/lib/oms_graph/child_policy.py`.

## Implementation map

| Modules | Responsibility and reused APIs |
| --- | --- |
| `cli.rs`, `cli.json` | Parsed CLI grammar, defaults, required options, help and usage captured from the Python parser |
| `project_build.rs`, `project_sqlite.rs` | Safe source discovery, SHA256 cache, freshness, atomic JSON and SQLite publication |
| `project_index.rs`, `project_analytics.rs` | Find, API, neighbors, trace, search, ranked symbol blocks, symbol zones, unreferenced candidates, assurance |
| `project_context.rs`, `project_history.rs`, `project_affected.rs` | Context packs and signature maps, Git coupling, fail-open affected plans and impact briefs |
| `project_visual.rs`, `graph.html` | Schema-3 project visual models and the existing interactive HTML template |
| `notebook.rs` | JSON code-cell extraction and physical line remapping through the isolated Python parser |
| `execution.rs`, `execution_validate.rs`, `execution_route.rs` | Lossless specs, predicates, validation, bindings, proofs, joins, nested routing and budgets |
| `execution_events.rs`, `execution_scheduler.rs`, `execution_workspace.rs` | Locked event append, event replay, run storage, task/scope scheduling and workspace-bound read caches |
| `execution_runner.rs`, `execution_commit.rs`, `execution_render.rs` | Plan projections, tool execution, recovery, gate decisions, exact-commit adapter, text/Mermaid rendering |
| `util.rs` | Physical paths, safe storage adapters, bounded process-group supervision, scrubbing and glob matching |

`oms-graph::parse`, `oms-graph-python::Session`, the shell callback selected by
`oms-graph`, and `oms-graph-resolve::resolve` supply parsing and resolution.
`oms-command-plan::{Context, Plan, Request}` supply plan reads and its `Store`
supplies state writes. The operations value type uses `oms-context` and
`oms-json` for Python JSON numbers; decimal ordering reuses
`oms-command-task::packet::decimal_cmp`. Source acquisition uses
`oms-safe-fs::Root`, state exclusion uses `oms-lock`, and displayed source and
artifact tails use `oms-scrub`. All path dependencies requested by the worker
brief are declared; there is no upstream API change in this patch.

`cli.json` was captured by walking `build_parser()` and serializing each
parser's options, children, help and usage. `graph.html` is the literal
`template` assignment in the legacy renderer's `_html_fragment` function.
Changes to those Python contracts require updating the corresponding native
asset and reviewing callers.

## Concurrency and interruption

| Written artifact | Concurrent call or interruption behavior |
| --- | --- |
| `.oms/.gitignore` | The atomic preserve-existing operation keeps an existing marker unchanged. A failed creation can leave private parent directories. |
| `cache/<digest>.json` | Entries are regenerable and atomically replaced. The project build lock excludes another native build. A killed build may leave reusable entries. |
| `graph.json`, `index.sqlite3`, `manifest.json` | Each publication is atomic, under the project build lock. SQLite is constructed in memory. Manifest publication follows graph and SQLite publication; a kill between them leaves a revision mismatch that freshness checking rejects. JSON is the query authority. |
| `orientation.json` | Derived after manifest publication. A missing orientation can be regenerated; it grants no authority. |
| `context/<digest>.json` | Content-addressed atomic writes can be repeated. A kill leaves an older complete file or no published file. |
| `runs/<run>/graph.json`, `events.jsonl` | The event lock protects new-run exclusion, sequence allocation and event append. A kill between spec publication and its first event leaves a frozen run that can be inspected and resumed. |
| `runs/<run>/projection.json` | Atomic, derived from the authoritative event stream, and replayable after a kill. |
| `runs/<run>/artifacts/*.txt` | Atomic bounded output tails. A kill before the outcome event may leave an orphan artifact; recovery reads plan state and blocks uncertain write tools. |
| `.oms/graph/cache/<digest>.json` | Atomic regenerable read-tool results. The key covers the complete safe dirty-workspace fingerprint and upstream outcomes. Unreadable, oversized or linked dirty paths disable cache reads and writes. |
| HTML output path | Atomic replacement of the explicitly selected output. The child-action policy rejects this write for harness children. |
| Exact commit | The existing goal-drive transaction owns index/ref writes and recovery; this adapter checks the bound task and landing receipt before invoking it. |

Native runner calls hold a separate run lock while coordinating concurrent
waves; only the coordinator appends events. Gate
decisions serialize their event append, and duplicate idempotency keys refuse
a conflicting concurrent decision. Process supervision observes an unreaped
child before stopping its process group; it does not signal an already reaped
process identity. Cancellation stops owned subprocess groups and the parser
session.

## Remaining parity work

The following are known gaps, not verified equivalents of the Python engine:

- Notebook validity uses a contained declaration probe through the existing
  parser, with separate handling for future-import records. Continuation
  filtering tracks quotes, brackets and line joining. Exact tokenizer parity
  for complex f-strings, parser-version extensions and helper-size boundaries
  remains unverified; no helper API extension is required by this adapter.
- `project context --bundle` explicitly refuses: the required canonical
  context compiler, phase rules and evidence-envelope service are not exposed
  by the allowed dependencies. Context packs and signature maps are implemented.
- Affected-test planning conservatively follows extracted dependencies, but
  does not yet reproduce diff-hunk case narrowing, selectable oversized suites
  or runner-membership edge cuts. Impact includes bounded CODEOWNERS attribution.
- The benchmark module and recall snapshot/comparison entrypoints are pending.
- Journal mirroring and all event field/details from the legacy runner remain
  to be matched. Completion events are currently recorded in selection order
  after a concurrent wave joins. Plan reads use `UnavailableServices`; plan records needing
  other producer projections require parent service wiring.
- Native capabilities use their own operation bounds. Matching a node-level
  deadline across every in-process capability remains pending.
- Some text formats, error details, deeply nested validation diagnostics and
  fixture coercions differ. Lexical matching uses Unicode lowercase rather
  than Python's full case folding. CLI integer storage is bounded to `i64`;
  execution JSON and pure route budgets retain arbitrary-size integers.
- Source discovery currently retains the selected source bytes during a build
  and caps one source read at 64 MiB. Large-repository memory and higher source
  caps need further work. Helper source limits also apply.
- The SQLite file is a derived index; readers currently query the JSON graph.
  Extraction cache pruning is pending. Execution HTML model parity is pending;
  project HTML reuses the legacy interactive asset.

These gaps preclude declaring the complete native migration ready. The write
scope excludes root service wiring, the dispatcher and workspace manifest,
so those integration changes belong to the owner.

## Verification phase

The worker brief limits this phase to Rust compilation and formatting. Cargo
check and rustfmt check are the verification commands; behavioral suites,
branch/limit edge cases, failure-under-reversion checks and cross-platform
execution were not run. No existing tests were changed or removed.
Legacy test inspection was scoped to validation, routing, parser, freshness,
concurrency and cache contracts; this was not an exhaustive source/test audit.

Relevant legacy contracts include `tests/test_oms_graph_integration.py`,
`tests/test_oms_graph_project.py`, `tests/test_oms_graph_parsers.py`,
`tests/graph-smoke.sh`, `docs/GRAPH-ENGINEERING.md`, and the project-graph
consumers in `oms-mcp-server` and `scripts/oms_project_graph_*`.
